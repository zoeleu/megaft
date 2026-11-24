//! HTTP client for the intra web endpoints.

use jiff::{Timestamp, ToSpan as _, ZonedDateTime, civil::Date};
use reqwest::{StatusCode, header};

use crate::{
    Error, Result, Session,
    parse::{self, CalendarAttrs},
    slots::SlotBlock,
};

/// `https://projects.intra.42.fr`
pub const BASE: &str = "https://projects.intra.42.fr";

pub use parse::ProjectEntry as Project;

/// Everything needed to list and book slots for one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookingPage {
    pub slug: String,
    pub calendar: CalendarAttrs,
}

impl BookingPage {
    fn referer(&self) -> String {
        format!(
            "{BASE}/projects/{}/slots?team_id={}",
            self.slug, self.calendar.team_id
        )
    }
}

/// Outcome of a booking request that the intra answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookResult {
    Booked {
        message: String,
    },
    Refused {
        status: u16,
        message: String,
    },
    /// The CSRF token went stale; reload the booking page and try again.
    StaleCsrf,
}

/// The `start`/`end` dates the web client sends with slot requests.
#[derive(Debug, Clone, Copy)]
pub struct DateRange {
    pub start: Date,
    pub end: Date,
}

impl DateRange {
    /// From today (local) to the day after `until` (local).
    #[must_use]
    pub fn covering(now: &ZonedDateTime, until: Timestamp) -> Self {
        let tz = now.time_zone().clone();
        let end = until
            .to_zoned_datetime(tz)
            .date()
            .checked_add(1.day())
            .unwrap_or_else(|_| now.date());
        Self {
            start: now.date(),
            end,
        }
    }

    fn query(self) -> [(&'static str, String); 2] {
        [("start", self.start.to_string()), ("end", self.end.to_string())]
    }
}

/// A logged-in intra web client. Cheap to clone.
#[derive(Debug, Clone)]
pub struct IntraWeb {
    http: reqwest::Client,
    session: Session,
}

struct Page {
    status: StatusCode,
    url: String,
    body: String,
}

impl IntraWeb {
    pub fn new(session: Session) -> Result<Self> {
        let http = reqwest::Client::builder()
            .cookie_provider(session.cookies.clone())
            .user_agent(session.user_agent.clone())
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self { http, session })
    }

    #[must_use]
    pub const fn session(&self) -> &Session {
        &self.session
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Page> {
        let resp = req.send().await?;
        let status = resp.status();
        let url = resp.url().to_string();
        let body = resp.text().await?;
        if url.contains("signin.intra.42.fr")
            || url.contains("auth.42.fr")
            || status == StatusCode::UNAUTHORIZED
        {
            return Err(Error::LoggedOut);
        }
        if parse::is_cloudflare_challenge(status.as_u16(), &body) {
            return Err(Error::Cloudflare);
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(Error::RateLimited);
        }
        Ok(Page { status, url, body })
    }

    async fn get_html(&self, path: &str) -> Result<Page> {
        self.send(self.http.get(format!("{BASE}{path}"))).await
    }

    fn json_headers(&self, req: reqwest::RequestBuilder, page: &BookingPage) -> reqwest::RequestBuilder {
        let req = req
            .header(header::ACCEPT, "application/json, text/javascript, */*; q=0.01")
            .header("X-Requested-With", "XMLHttpRequest")
            .header(header::REFERER, page.referer());
        match &page.calendar.csrf_token {
            Some(t) => req.header("X-CSRF-Token", t),
            None => req,
        }
    }

    /// Login of the session owner, or `None` if logged out.
    pub async fn whoami(&self) -> Result<Option<String>> {
        match self.get_html("/").await {
            Ok(p) => Ok(parse::whoami(&p.body)),
            Err(Error::LoggedOut) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Projects with an open team (home page).
    pub async fn open_projects(&self) -> Result<Vec<Project>> {
        parse::open_projects(&self.get_html("/").await?.body)
    }

    /// Evaluations already booked for you on this project.
    pub async fn upcoming_evaluations(&self, slug: &str) -> Result<Vec<Timestamp>> {
        parse::upcoming_evaluations(&self.get_html(&format!("/{slug}/mine")).await?.body)
    }

    /// Loads the booking page. Uses the team id the page itself picks unless `team_id` overrides it.
    pub async fn booking_page(&self, slug: &str, team_id: Option<&str>) -> Result<BookingPage> {
        let q = team_id.map(|t| format!("?team_id={t}")).unwrap_or_default();
        let page = self.get_html(&format!("/projects/{slug}/slots{q}")).await?;
        if !page.status.is_success() {
            return Err(Error::Status {
                status: page.status.as_u16(),
                url: page.url,
            });
        }
        let calendar = parse::calendar(&page.body)?.ok_or_else(|| Error::NoCalendar(slug.to_owned()))?;
        Ok(BookingPage {
            slug: slug.to_owned(),
            calendar,
        })
    }

    /// Open slot blocks in `range`.
    pub async fn slots(&self, page: &BookingPage, range: DateRange) -> Result<Vec<SlotBlock>> {
        let req = self.json_headers(self.http.get(format!("{BASE}{}", page.calendar.index_url)), page);
        let resp = self.send(req.query(&range.query())).await?;
        if !resp.status.is_success() {
            return Err(Error::Status {
                status: resp.status.as_u16(),
                url: resp.url,
            });
        }
        serde_json::from_str(&resp.body)
            .map_err(|e| Error::Unexpected(format!("slots.json: {e} (session expired?)")))
    }

    /// Books the evaluation made of `ids` (comma-separated, as in [`crate::Candidate::ids`]).
    pub async fn book(&self, page: &BookingPage, ids: &str, range: DateRange) -> Result<BookResult> {
        let url = format!("{BASE}{}", page.calendar.update_url.replace(":ids", ids));
        let [start, end] = range.query();
        let form = [start, end, ("_method", "put".to_owned())];
        let resp = self
            .send(self.json_headers(self.http.post(url), page).form(&form))
            .await?;
        let json: Option<serde_json::Value> = serde_json::from_str(&resp.body).ok();
        let message = json
            .as_ref()
            .and_then(|j| j.get("message"))
            .and_then(serde_json::Value::as_str)
            .map_or_else(|| resp.body.chars().take(200).collect(), str::to_owned);
        let inner = json
            .as_ref()
            .and_then(|j| j.get("status"))
            .and_then(serde_json::Value::as_u64);
        if resp.status.is_success() && inner.is_none_or(|s| s == 200) {
            return Ok(BookResult::Booked { message });
        }
        if resp.status == StatusCode::UNPROCESSABLE_ENTITY
            && resp.body.to_lowercase().contains("authenticity")
        {
            return Ok(BookResult::StaleCsrf);
        }
        Ok(BookResult::Refused {
            status: resp.status.as_u16(),
            message,
        })
    }
}
