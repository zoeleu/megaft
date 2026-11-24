//! The booking loop: poll for slots, book up to `count` evaluations before the deadline.

use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use jiff::{SignedDuration, Timestamp, ZonedDateTime};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::{
    Error, IntraWeb, Result,
    intra::{BookResult, DateRange},
    slots::{self, Span},
};

/// What to book.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSpec {
    /// Project slug, e.g. `codexion`.
    pub project: String,
    /// How many evaluations to book.
    pub count: u32,
    /// Evaluations must *start* before this.
    pub deadline: Timestamp,
    /// …and not before this (default: now).
    pub not_before: Option<Timestamp>,
    /// Minimum minutes between now and the start of a booked slot.
    pub lead_minutes: u32,
    /// Minutes kept clear around existing evaluations and between this job's own bookings.
    pub buffer_minutes: u32,
    /// Seconds between polls (plus up to 30 % jitter).
    pub interval_secs: u32,
    /// Report what would be booked without booking.
    pub dry_run: bool,
    /// Extra ranges to stay clear of.
    #[serde(default)]
    pub avoid: Vec<(Timestamp, Timestamp)>,
    /// Override the team id the booking page picks.
    #[serde(default)]
    pub team_id: Option<String>,
}

impl JobSpec {
    /// Sensible defaults: 15 s polls, 15 min buffer, no lead.
    #[must_use]
    pub const fn new(project: String, count: u32, deadline: Timestamp) -> Self {
        Self {
            project,
            count,
            deadline,
            not_before: None,
            lead_minutes: 0,
            buffer_minutes: 15,
            interval_secs: 15,
            dry_run: false,
            avoid: Vec::new(),
            team_id: None,
        }
    }
}

/// A booked (or, in a dry run, found) evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Booked {
    pub start: Timestamp,
    pub end: Timestamp,
    pub dry_run: bool,
}

/// Progress reported while a job runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobEvent {
    Log(String),
    Booked(Booked),
}

/// Why a job stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobOutcome {
    /// All requested evaluations booked.
    Done,
    /// The deadline passed first.
    Deadline,
    Cancelled,
}

/// Gets a fresh logged-in client when the session expires or Cloudflare challenges us.
pub trait Refresher: Send + Sync {
    fn refresh<'a>(&'a self, why: &'a Error) -> BoxFuture<'a, Result<IntraWeb>>;
}

/// A refresher that can't refresh: the job fails when the session expires.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRefresh;

impl Refresher for NoRefresh {
    fn refresh<'a>(&'a self, why: &'a Error) -> BoxFuture<'a, Result<IntraWeb>> {
        let err = match why {
            Error::Cloudflare => Error::Cloudflare,
            _ => Error::LoggedOut,
        };
        Box::pin(async move { Err(err) })
    }
}

/// Don't launch a browser more often than this.
const MIN_REFRESH_GAP: Duration = Duration::from_secs(120);
const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(60);

struct Ctx<'a> {
    intra: IntraWeb,
    refresher: &'a dyn Refresher,
    last_refresh: Option<Instant>,
    events: &'a UnboundedSender<JobEvent>,
    cancel: &'a CancellationToken,
}

impl Ctx<'_> {
    fn log(&self, msg: impl Into<String>) {
        let msg = msg.into();
        tracing::info!("{msg}");
        if self.events.send(JobEvent::Log(msg)).is_err() {
            tracing::debug!("job event receiver dropped");
        }
    }

    /// Gets a new session after `why`, unless we just did (then `why` is returned).
    async fn refresh(&mut self, why: Error) -> Result<()> {
        if self.last_refresh.is_some_and(|t| t.elapsed() < MIN_REFRESH_GAP) {
            return Err(why);
        }
        self.log(format!("{why} → logging in again"));
        self.last_refresh = Some(Instant::now());
        self.intra = self.refresher.refresh(&why).await?;
        Ok(())
    }

    /// Sleeps unless cancelled; returns `false` if cancelled.
    async fn sleep(&self, d: Duration) -> bool {
        tokio::select! {
            () = self.cancel.cancelled() => false,
            () = tokio::time::sleep(d) => true,
        }
    }
}

/// `retrying!(ctx, |i| i.some_call(..))`: runs the call; if the session must be renewed, refreshes
/// once and runs it again.
macro_rules! retrying {
    ($ctx:expr, |$i:ident| $call:expr) => {{
        let first = {
            let $i = &$ctx.intra;
            $call.await
        };
        match first {
            Err(e) if e.needs_new_session() => match $ctx.refresh(e).await {
                Ok(()) => {
                    let $i = &$ctx.intra;
                    $call.await
                }
                Err(e) => Err(e),
            },
            other => other,
        }
    }};
}

fn spans_of(starts: &[Timestamp], duration: u32, pad: SignedDuration) -> Vec<Span> {
    let len = slots::evaluation_length(duration);
    starts
        .iter()
        .map(|s| {
            Span {
                start: *s,
                end: *s + len,
            }
            .padded(pad)
        })
        .collect()
}

fn jittered(interval_secs: u32) -> Duration {
    let base = u64::from(interval_secs).saturating_mul(1000);
    Duration::from_millis(base.saturating_add(rand::random_range(0..=base.saturating_mul(3) / 10)))
}

fn local(t: Timestamp) -> String {
    t.to_zoned_datetime(jiff::tz::TimeZone::system())
        .strftime("%a %d %H:%M")
        .to_string()
}

fn local_hm(t: Timestamp) -> String {
    t.to_zoned_datetime(jiff::tz::TimeZone::system())
        .strftime("%H:%M")
        .to_string()
}

/// Polls and books until `spec.count` evaluations are booked, the deadline passes, or `cancel` fires.
///
/// Existing evaluations on the project are avoided automatically (± `buffer_minutes`), and so are
/// this job's own bookings.
pub async fn run(
    intra: IntraWeb,
    refresher: &dyn Refresher,
    spec: &JobSpec,
    cancel: &CancellationToken,
    events: &UnboundedSender<JobEvent>,
) -> Result<(JobOutcome, Vec<Booked>)> {
    let mut ctx = Ctx {
        intra,
        refresher,
        last_refresh: None,
        events,
        cancel,
    };
    let pad = SignedDuration::from_mins(i64::from(spec.buffer_minutes));
    let lead = SignedDuration::from_mins(i64::from(spec.lead_minutes));
    let team = spec.team_id.clone();
    let slug = spec.project.clone();

    let mut page = retrying!(ctx, |i| i.booking_page(&slug, team.as_deref()))?;
    let duration = page.calendar.duration;
    let existing = retrying!(ctx, |i| i.upcoming_evaluations(&slug)).unwrap_or_else(|e| {
        ctx.log(format!("could not read existing evaluations ({e})"));
        Vec::new()
    });
    let mut avoid: Vec<Span> = spec
        .avoid
        .iter()
        .map(|(s, e)| Span { start: *s, end: *e })
        .collect();
    avoid.extend(spans_of(&existing, duration, pad));

    ctx.log(format!(
        "{slug}: team {}, evaluation = {duration} × {} min",
        page.calendar.team_id,
        slots::SLOT_MINUTES
    ));
    for s in &existing {
        ctx.log(format!(
            "already booked: {} (keeping {} min clear)",
            local(*s),
            spec.buffer_minutes
        ));
    }
    ctx.log(format!(
        "looking for {} slot(s) starting before {}",
        spec.count,
        local(spec.deadline)
    ));

    let mut booked: Vec<Booked> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    let target = usize::try_from(spec.count).unwrap_or(usize::MAX);

    while booked.len() < target {
        if cancel.is_cancelled() {
            return Ok((JobOutcome::Cancelled, booked));
        }
        let now = ZonedDateTime::now();
        if now.timestamp() >= spec.deadline {
            ctx.log(if booked.is_empty() {
                "deadline passed, nothing booked."
            } else {
                "deadline passed."
            });
            return Ok((JobOutcome::Deadline, booked));
        }
        let not_before = spec
            .not_before
            .unwrap_or_else(|| now.timestamp())
            .max(now.timestamp() + lead);
        let range = DateRange::covering(&now, spec.deadline);

        let blocks = match retrying!(ctx, |i| i.slots(&page, range)) {
            Ok(b) => b,
            Err(Error::RateLimited) => {
                ctx.log("rate limited, backing off 60 s");
                if !ctx.sleep(RATE_LIMIT_BACKOFF).await {
                    return Ok((JobOutcome::Cancelled, booked));
                }
                continue;
            }
            Err(e) if e.needs_new_session() => return Err(e),
            Err(e) => {
                ctx.log(format!("slots fetch failed: {e}; reloading the booking page"));
                page = retrying!(ctx, |i| i.booking_page(&slug, team.as_deref()))?;
                if !ctx.sleep(jittered(spec.interval_secs)).await {
                    return Ok((JobOutcome::Cancelled, booked));
                }
                continue;
            }
        };

        let choices: Vec<_> = slots::candidates(&blocks, duration, not_before, spec.deadline, &avoid)
            .into_iter()
            .filter(|c| !refused.contains(&c.ids))
            .collect();
        if choices.is_empty() {
            let open = blocks.iter().filter(|b| b.is_available()).count();
            ctx.log(format!("no slot in window ({open} blocks open overall)"));
        }

        for c in choices {
            if cancel.is_cancelled() {
                break;
            }
            let span = c.span;
            if spec.dry_run {
                ctx.log(format!(
                    "[dry-run] would book {}–{}",
                    local(span.start),
                    local_hm(span.end)
                ));
                let b = Booked {
                    start: span.start,
                    end: span.end,
                    dry_run: true,
                };
                avoid.push(span.padded(pad));
                if events.send(JobEvent::Booked(b.clone())).is_err() {
                    tracing::debug!("job event receiver dropped");
                }
                booked.push(b);
                if booked.len() >= target {
                    break;
                }
                continue;
            }
            ctx.log(format!("booking {}–{} …", local(span.start), local_hm(span.end)));
            let result = retrying!(ctx, |i| i.book(&page, &c.ids, range))?;
            match result {
                BookResult::Booked { message } => {
                    ctx.log(format!(
                        "BOOKED {slug} {}–{}: {message}",
                        local(span.start),
                        local_hm(span.end)
                    ));
                    let b = Booked {
                        start: span.start,
                        end: span.end,
                        dry_run: false,
                    };
                    if events.send(JobEvent::Booked(b.clone())).is_err() {
                        tracing::debug!("job event receiver dropped");
                    }
                    booked.push(b);
                    avoid.push(span.padded(pad));
                    if booked.len() < target {
                        match retrying!(ctx, |i| i.booking_page(&slug, team.as_deref())) {
                            Ok(p) => page = p,
                            Err(Error::NoCalendar(_)) => {
                                ctx.log("the intra won't take more bookings for this project");
                                return Ok((JobOutcome::Done, booked));
                            }
                            Err(e) => return Err(e),
                        }
                        if let Ok(more) = retrying!(ctx, |i| i.upcoming_evaluations(&slug)) {
                            avoid.extend(spans_of(&more, duration, pad));
                        }
                    }
                    break; // fresh slot list before the next booking
                }
                BookResult::StaleCsrf => {
                    ctx.log("stale CSRF token, reloading the booking page");
                    page = retrying!(ctx, |i| i.booking_page(&slug, team.as_deref()))?;
                    break;
                }
                BookResult::Refused { status, message } => {
                    ctx.log(format!("booking refused (HTTP {status}): {message}"));
                    refused.push(c.ids);
                }
            }
        }
        if booked.len() < target && !ctx.sleep(jittered(spec.interval_secs)).await {
            return Ok((JobOutcome::Cancelled, booked));
        }
    }
    Ok((JobOutcome::Done, booked))
}
