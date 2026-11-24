//! Logs in to the 42 intra with a real Chrome and returns the session cookies (incl. Cloudflare's
//! `cf_clearance`) and the user agent they're bound to.
//!
//! Chrome runs in a throwaway profile under `$XDG_RUNTIME_DIR` (deleted afterwards), so nothing of
//! the session is left on disk except what the caller saves. The password, if given, only lives in
//! memory (`SecretString`).

use std::{path::PathBuf, time::Duration};

use chromiumoxide::{
    Browser, BrowserConfig, Page,
    cdp::browser_protocol::network::{Cookie, CookieParam, TimeSinceEpoch},
};
use futures::StreamExt as _;
use megaft_core::BrowserCookie;
use secrecy::{ExposeSecret as _, SecretString};

const TARGET: &str = "https://projects.intra.42.fr/";

/// What can go wrong logging in.
#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error("no Chrome/Chromium found (set MEGAFT_CHROME)")]
    NoChrome,
    #[error("Chrome: {0}")]
    Chrome(String),
    #[error("login refused: {0}")]
    BadCredentials(String),
    #[error("not logged in after {secs} s (stuck at {url})")]
    Timeout { secs: u64, url: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<chromiumoxide::error::CdpError> for BrowserError {
    fn from(e: chromiumoxide::error::CdpError) -> Self {
        Self::Chrome(e.to_string())
    }
}

type Result<T, E = BrowserError> = std::result::Result<T, E>;

/// Login and password, kept in memory only.
#[derive(Debug, Clone)]
pub struct Credentials {
    pub login: String,
    pub password: SecretString,
}

/// How to run the login.
#[derive(Debug, Clone)]
pub struct LoginOptions {
    /// Show the window (for 2FA or a manual login).
    pub visible: bool,
    /// Ignore the saved session so the password is really used (and checked).
    pub fresh: bool,
    /// How long to wait for the intra to show us logged in.
    pub timeout: Duration,
}

impl LoginOptions {
    #[must_use]
    pub const fn headless() -> Self {
        Self {
            visible: false,
            fresh: false,
            timeout: Duration::from_secs(45),
        }
    }

    #[must_use]
    pub const fn visible() -> Self {
        Self {
            visible: true,
            fresh: false,
            timeout: Duration::from_secs(300),
        }
    }
}

/// A logged-in session.
#[derive(Debug, Clone)]
pub struct LoggedIn {
    pub login: String,
    pub cookies: Vec<BrowserCookie>,
    pub user_agent: String,
}

fn find_chrome() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("MEGAFT_CHROME") {
        return Ok(PathBuf::from(p));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    for name in [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
    ] {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(BrowserError::NoChrome)
}

fn to_param(c: &BrowserCookie) -> Result<CookieParam> {
    let mut b = CookieParam::builder()
        .name(c.name.clone())
        .value(c.value.clone())
        .domain(c.domain.clone())
        .path(c.path.clone())
        .secure(c.secure)
        .http_only(c.http_only);
    if let Some(exp) = c.expires {
        b = b.expires(TimeSinceEpoch::new(f64::from(
            i32::try_from(exp).unwrap_or(i32::MAX),
        )));
    }
    b.build().map_err(BrowserError::Chrome)
}

fn from_cdp(c: &Cookie) -> BrowserCookie {
    // CDP reports expiry as fractional seconds; -1 for session cookies.
    let expires = (!c.session && c.expires > 0.0)
        .then(|| format!("{:.0}", c.expires.trunc()).parse().ok())
        .flatten();
    BrowserCookie {
        name: c.name.clone(),
        value: c.value.clone(),
        domain: c.domain.trim_start_matches('.').to_owned(),
        path: c.path.clone(),
        secure: c.secure,
        http_only: c.http_only,
        expires,
    }
}

fn is_42(domain: &str) -> bool {
    let d = domain.trim_start_matches('.');
    d == "42.fr" || d.ends_with(".42.fr")
}

async fn eval_string(page: &Page, js: &str) -> Option<String> {
    page.evaluate(js)
        .await
        .ok()?
        .into_value::<Option<String>>()
        .ok()
        .flatten()
}

async fn logged_in_as(page: &Page) -> Option<String> {
    let url = page.url().await.ok().flatten()?;
    if !url.starts_with(TARGET) {
        return None;
    }
    eval_string(
        page,
        "window._user && window._user.login ? window._user.login : null",
    )
    .await
}

async fn fill_login_form(page: &Page, creds: &Credentials) -> Result<()> {
    page.find_element("#username")
        .await?
        .click()
        .await?
        .type_str(&creds.login)
        .await?;
    page.find_element("#password")
        .await?
        .click()
        .await?
        .type_str(creds.password.expose_secret())
        .await?;
    if let Ok(remember) = page.find_element("#rememberMe").await
        && remember.click().await.is_err()
    {
        tracing::debug!("couldn't tick 'remember me'");
    }
    page.find_element("#kc-login").await?.click().await?;
    tracing::info!("submitted the login form");
    Ok(())
}

async fn login_error(page: &Page) -> Option<String> {
    eval_string(
        page,
        r##"(() => { for (const s of ["#input-error", ".kc-feedback-text", ".alert-error", ".pf-m-danger"]) {
               const e = document.querySelector(s); if (e && e.innerText.trim()) return e.innerText.trim(); }
             return null; })()"##,
    )
    .await
}

/// Opens the intra in Chrome, makes sure we're logged in, and returns the session.
///
/// `saved` is loaded into the browser first (unless `opts.fresh`); `creds` fill the login form if
/// the intra asks for it.
pub async fn login(
    saved: &[BrowserCookie],
    creds: Option<&Credentials>,
    opts: &LoginOptions,
) -> Result<LoggedIn> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let profile = tempfile::Builder::new()
        .prefix("megaft-chrome-")
        .tempdir_in(runtime)?;
    let mut cfg = BrowserConfig::builder()
        .chrome_executable(find_chrome()?)
        .user_data_dir(profile.path())
        .hide()
        .window_size(1200, 900)
        .launch_timeout(Duration::from_secs(30))
        .args([
            "--no-first-run",
            "--password-store=basic",
            "--disable-blink-features=AutomationControlled",
        ]);
    cfg = if opts.visible {
        cfg.with_head()
    } else {
        cfg.new_headless_mode()
    };
    let (mut browser, mut handler) = Browser::launch(cfg.build().map_err(BrowserError::Chrome)?).await?;
    let pump = tokio::spawn(async move {
        while let Some(ev) = handler.next().await {
            if let Err(e) = ev {
                tracing::debug!("chrome handler: {e}");
            }
        }
    });

    let result = drive(&browser, saved, creds, opts).await;

    if let Err(e) = browser.close().await {
        tracing::debug!("closing chrome: {e}");
    }
    if let Err(e) = browser.wait().await {
        tracing::debug!("waiting for chrome: {e}");
    }
    pump.abort();
    drop(profile); // deletes the throwaway profile
    result
}

async fn drive(
    browser: &Browser,
    saved: &[BrowserCookie],
    creds: Option<&Credentials>,
    opts: &LoginOptions,
) -> Result<LoggedIn> {
    if !opts.fresh && !saved.is_empty() {
        let params = saved.iter().map(to_param).collect::<Result<Vec<_>>>()?;
        browser.set_cookies(params).await?;
    }
    let page = browser.new_page(TARGET).await?;
    let start = tokio::time::Instant::now();
    let mut submitted = false;
    let login = loop {
        if let Some(who) = logged_in_as(&page).await {
            break who;
        }
        let url = page.url().await.ok().flatten().unwrap_or_default();
        if url.contains("auth.42.fr") {
            match creds {
                Some(c) if !submitted => {
                    // The form may still be loading; retry on the next tick if it isn't there yet.
                    submitted = fill_login_form(&page, c).await.is_ok();
                }
                Some(_) => {
                    if let Some(err) = login_error(&page).await {
                        return Err(BrowserError::BadCredentials(err));
                    }
                }
                None => {}
            }
        }
        if start.elapsed() > opts.timeout {
            return Err(BrowserError::Timeout {
                secs: opts.timeout.as_secs(),
                url,
            });
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
    };
    let cookies = browser
        .get_cookies()
        .await?
        .iter()
        .filter(|c| is_42(&c.domain))
        .map(from_cdp)
        .collect();
    let user_agent = eval_string(&page, "navigator.userAgent")
        .await
        .unwrap_or_default()
        .replace("HeadlessChrome", "Chrome");
    Ok(LoggedIn {
        login,
        cookies,
        user_agent,
    })
}

/// Renews the intra session with Chrome when a job is logged out or challenged by Cloudflare.
///
/// Tries headless first (saved session, then the in-memory password); falls back to a visible
/// window if `allow_visible`. The new session is saved to `session_path`.
#[derive(Debug, Clone)]
pub struct BrowserRefresher {
    pub session_path: PathBuf,
    pub credentials: Option<Credentials>,
    pub allow_visible: bool,
}

impl BrowserRefresher {
    /// Logs in and returns the updated session (also saved to disk).
    pub async fn renew(
        &self,
        current: &megaft_core::Session,
    ) -> Result<(megaft_core::Session, String), RefreshError> {
        let saved = current.export()?;
        let mut attempt = login(&saved, self.credentials.as_ref(), &LoginOptions::headless()).await;
        if let Err(e) = &attempt
            && self.allow_visible
            && !matches!(e, BrowserError::NoChrome)
        {
            tracing::warn!("headless login failed ({e}); opening a visible window");
            attempt = login(&saved, self.credentials.as_ref(), &LoginOptions::visible()).await;
        }
        let logged = attempt?;
        let mut session = current.clone();
        session.replace(logged.cookies, logged.user_agent)?;
        session.save(&self.session_path)?;
        Ok((session, logged.login))
    }
}

/// Errors from [`BrowserRefresher::renew`].
#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    #[error(transparent)]
    Browser(#[from] BrowserError),
    #[error(transparent)]
    Core(#[from] megaft_core::Error),
}

/// A [`megaft_core::Refresher`] backed by [`BrowserRefresher`].
#[derive(Debug)]
pub struct JobRefresher {
    pub browser: BrowserRefresher,
    pub session: tokio::sync::Mutex<megaft_core::Session>,
}

impl megaft_core::Refresher for JobRefresher {
    fn refresh<'a>(
        &'a self,
        _why: &'a megaft_core::Error,
    ) -> futures::future::BoxFuture<'a, megaft_core::Result<megaft_core::IntraWeb>> {
        Box::pin(async move {
            let mut current = self.session.lock().await;
            let (session, _) = self.browser.renew(&current).await.map_err(|e| match e {
                RefreshError::Core(e) => e,
                RefreshError::Browser(e) => megaft_core::Error::Unexpected(format!("browser login: {e}")),
            })?;
            *current = session.clone();
            megaft_core::IntraWeb::new(session)
        })
    }
}
