//! Error type for the intra client.

/// Everything that can go wrong talking to the intra.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The session cookies no longer work (redirected to the login page).
    #[error("not logged in to the intra (session expired)")]
    LoggedOut,
    /// Cloudflare answered with a challenge page instead of the intra.
    #[error("Cloudflare challenge: the cf_clearance cookie needs refreshing")]
    Cloudflare,
    /// The project page has no booking calendar (not waiting for an evaluation, or wrong slug).
    #[error("no booking calendar for project `{0}` (is it waiting for an evaluation?)")]
    NoCalendar(String),
    /// The intra answered, but not with what we expected.
    #[error("unexpected intra response: {0}")]
    Unexpected(String),
    /// HTTP status that isn't handled more specifically.
    #[error("HTTP {status} from {url}")]
    Status { status: u16, url: String },
    /// Rate limited by the intra (HTTP 429).
    #[error("rate limited by the intra")]
    RateLimited,
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("time: {0}")]
    Time(#[from] jiff::Error),
    #[error("cookie store: {0}")]
    Cookies(String),
}

impl Error {
    /// Whether a fresh browser login could fix this error.
    #[must_use]
    pub const fn needs_new_session(&self) -> bool {
        matches!(self, Self::LoggedOut | Self::Cloudflare)
    }
}

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;
