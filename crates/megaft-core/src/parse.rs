//! HTML parsing for the intra web pages. Pure functions over page text, tested with fixtures in
//! `tests/fixtures/` (trimmed copies of the real markup with personal data removed).

use jiff::Timestamp;
use scraper::{Html, Selector};

use crate::{Error, Result};

/// What the booking page tells us (`/projects/<slug>/slots?team_id=`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarAttrs {
    pub team_id: String,
    /// Number of 15-minute slots in one evaluation.
    pub duration: u32,
    /// e.g. `/projects/codexion/slots.json?team_id=7808307`
    pub index_url: String,
    /// e.g. `/projects/codexion/slots/:ids.json?team_id=7808307`
    pub update_url: String,
    pub csrf_token: Option<String>,
}

/// A project with an open team, from the intra home page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectEntry {
    pub slug: String,
    pub name: String,
    /// e.g. "Evaluation needed", "In progress".
    pub status: String,
}

fn selector(css: &str) -> Result<Selector> {
    Selector::parse(css).map_err(|e| Error::Unexpected(format!("bad selector {css}: {e}")))
}

fn text_of(el: scraper::ElementRef<'_>) -> String {
    el.text()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parses the `#calendar` element and the CSRF meta tag. `None` if the page has no calendar.
pub fn calendar(html: &str) -> Result<Option<CalendarAttrs>> {
    let doc = Html::parse_document(html);
    let Some(cal) = doc.select(&selector("#calendar")?).next() else {
        return Ok(None);
    };
    let attr = |name: &str| {
        cal.value()
            .attr(name)
            .map(str::to_owned)
            .ok_or_else(|| Error::Unexpected(format!("#calendar has no {name}")))
    };
    let index_url = attr("data-index-url")?;
    let update_url = attr("data-update-url")?;
    let team_id = query_param(&index_url, "team_id")
        .ok_or_else(|| Error::Unexpected(format!("no team_id in {index_url}")))?;
    let duration = cal
        .value()
        .attr("data-duration")
        .and_then(|d| d.parse().ok())
        .unwrap_or(2);
    let csrf_token = doc
        .select(&selector(r#"meta[name="csrf-token"]"#)?)
        .next()
        .and_then(|m| m.value().attr("content"))
        .map(str::to_owned);
    Ok(Some(CalendarAttrs {
        team_id,
        duration,
        index_url,
        update_url,
        csrf_token,
    }))
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let (_, query) = url.split_once('?')?;
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| v.to_owned())
    })
}

/// The open teams on the home page: `<a class="simple-link" href="/<slug>/<login>">`.
pub fn open_projects(html: &str) -> Result<Vec<ProjectEntry>> {
    let doc = Html::parse_document(html);
    let (b, small) = (selector("b")?, selector("small")?);
    let mut out = Vec::new();
    for a in doc.select(&selector("a.simple-link[href]")?) {
        let Some(slug) = a
            .value()
            .attr("href")
            .and_then(|h| h.trim_start_matches('/').split('/').next())
        else {
            continue;
        };
        if slug.is_empty() {
            continue;
        }
        out.push(ProjectEntry {
            slug: slug.to_owned(),
            name: a
                .select(&b)
                .next()
                .map(text_of)
                .unwrap_or_else(|| slug.to_owned()),
            status: a.select(&small).next().map(text_of).unwrap_or_default(),
        });
    }
    Ok(out)
}

/// Start times of evaluations where *you* are evaluated, from `/<slug>/mine`.
pub fn upcoming_evaluations(html: &str) -> Result<Vec<Timestamp>> {
    let doc = Html::parse_document(html);
    let date = selector("[data-long-date]")?;
    let mut out = Vec::new();
    for item in doc.select(&selector(".project-item.reminder")?) {
        if !text_of(item).contains("You will be evaluated") {
            continue;
        }
        if let Some(d) = item
            .select(&date)
            .next()
            .and_then(|e| e.value().attr("data-long-date"))
        {
            out.push(parse_long_date(d)?);
        }
    }
    out.sort();
    Ok(out)
}

/// `2026-11-23 23:00:00 -0300`
pub fn parse_long_date(s: &str) -> Result<Timestamp> {
    Ok(jiff::fmt::strtime::parse("%Y-%m-%d %H:%M:%S %z", s)?.to_timestamp()?)
}

/// The login of whoever owns the session: `this._user = { "login": "…", … }` in every intra page.
#[must_use]
pub fn whoami(html: &str) -> Option<String> {
    let (_, rest) = html.split_once(r#""login":"#)?;
    let rest = rest.trim_start().strip_prefix('"')?;
    let (login, _) = rest.split_once('"')?;
    (!login.is_empty()).then(|| login.to_owned())
}

/// Whether a response body is a Cloudflare challenge rather than the intra.
#[must_use]
pub fn is_cloudflare_challenge(status: u16, body: &str) -> bool {
    matches!(status, 403 | 503)
        && (body.contains("Just a moment") || body.contains("cf-chl") || body.contains("challenge-platform"))
}
