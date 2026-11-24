//! The intra session: cookies (incl. Cloudflare's `cf_clearance`) and the user agent they're bound to.
//!
//! Stored in `$XDG_STATE_HOME/megaft/session.json` (home, mode 0600), never on sgoinfre.

use std::{
    io::{BufRead as _, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
};

use cookie_store::{CookieStore, RawCookie};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Used when no browser has given us a user agent yet.
pub const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/158.0.0.0 Safari/537.36";

/// Cookies + the user agent that earned them. Cheap to clone (shared store).
#[derive(Debug, Clone)]
pub struct Session {
    pub cookies: Arc<reqwest_cookie_store::CookieStoreRwLock>,
    pub user_agent: String,
}

#[derive(Serialize, Deserialize)]
struct OnDisk {
    user_agent: String,
    cookies: serde_json::Value,
}

impl Session {
    /// An empty session (not logged in).
    #[must_use]
    pub fn empty() -> Self {
        Self::from_store(CookieStore::default(), DEFAULT_USER_AGENT.to_owned())
    }

    fn from_store(store: CookieStore, user_agent: String) -> Self {
        Self {
            cookies: Arc::new(reqwest_cookie_store::CookieStoreRwLock::new(store)),
            user_agent,
        }
    }

    /// `$XDG_STATE_HOME/megaft/session.json`, falling back to `~/.local/state`.
    #[must_use]
    pub fn default_path() -> PathBuf {
        state_dir().join("session.json")
    }

    /// Loads a saved session; `Ok(None)` if there is none.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let disk: OnDisk = serde_json::from_slice(&data)?;
        let store = cookie_store::serde::json::load_all(serde_json::to_vec(&disk.cookies)?.as_slice())
            .map_err(|e| Error::Cookies(e.to_string()))?;
        Ok(Some(Self::from_store(store, disk.user_agent)))
    }

    /// Saves atomically with mode 0600 (session cookies are as good as a password).
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut buf = Vec::new();
        {
            let store = self.cookies.read().map_err(|e| Error::Cookies(e.to_string()))?;
            cookie_store::serde::json::save_incl_expired_and_nonpersistent(&store, &mut buf)
                .map_err(|e| Error::Cookies(e.to_string()))?;
        }
        let disk = OnDisk {
            user_agent: self.user_agent.clone(),
            cookies: serde_json::from_slice(&buf)?,
        };
        write_private(path, &serde_json::to_vec(&disk)?)
    }

    /// Replaces all cookies (after a browser login).
    pub fn replace(&mut self, cookies: Vec<BrowserCookie>, user_agent: String) -> Result<()> {
        let mut store = CookieStore::default();
        for c in cookies {
            insert(&mut store, &c)?;
        }
        *self.cookies.write().map_err(|e| Error::Cookies(e.to_string()))? = store;
        self.user_agent = user_agent;
        Ok(())
    }

    /// All 42 cookies, for loading into a browser.
    pub fn export(&self) -> Result<Vec<BrowserCookie>> {
        let store = self.cookies.read().map_err(|e| Error::Cookies(e.to_string()))?;
        Ok(store
            .iter_unexpired()
            .map(|c| BrowserCookie {
                name: c.name().to_owned(),
                value: c.value().to_owned(),
                domain: c
                    .domain()
                    .map(|d| d.trim_start_matches('.').to_owned())
                    .unwrap_or_default(),
                path: c.path().unwrap_or("/").to_owned(),
                secure: c.secure().unwrap_or(false),
                http_only: c.http_only().unwrap_or(false),
                expires: c.expires_datetime().map(|t| t.unix_timestamp()),
            })
            .collect())
    }

    /// Imports a Netscape `cookies.txt` (e.g. the Python evalfinder's `cookies42.txt` + `.ua` file).
    pub fn import_netscape(path: &Path, user_agent: Option<String>) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        let mut store = CookieStore::default();
        for line in std::io::BufReader::new(file).lines() {
            let line = line?;
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split('\t').collect();
            let [domain, _, path, secure, expires, name, value] = f.as_slice() else {
                continue;
            };
            insert(
                &mut store,
                &BrowserCookie {
                    name: (*name).to_owned(),
                    value: (*value).to_owned(),
                    domain: domain.trim_start_matches('.').to_owned(),
                    path: (*path).to_owned(),
                    secure: *secure == "TRUE",
                    http_only: false,
                    expires: expires.parse::<i64>().ok().filter(|e| *e > 0),
                },
            )?;
        }
        let ua_file = path.with_extension("txt.ua");
        let ua = user_agent
            .or_else(|| std::fs::read_to_string(ua_file).ok().map(|s| s.trim().to_owned()))
            .unwrap_or_else(|| DEFAULT_USER_AGENT.to_owned());
        Ok(Self::from_store(store, ua))
    }
}

/// A cookie as exchanged with the browser (CDP) and stored on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserCookie {
    pub name: String,
    pub value: String,
    /// Without leading dot.
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    /// Unix seconds; `None` for session cookies.
    pub expires: Option<i64>,
}

fn insert(store: &mut CookieStore, c: &BrowserCookie) -> Result<()> {
    let mut raw = RawCookie::build((c.name.clone(), c.value.clone()))
        .domain(c.domain.clone())
        .path(c.path.clone())
        .secure(c.secure)
        .http_only(c.http_only);
    if let Some(exp) = c.expires
        && let Ok(t) = time::OffsetDateTime::from_unix_timestamp(exp)
    {
        raw = raw.expires(t);
    }
    let url = url::Url::parse(&format!("https://{}{}", c.domain, c.path))
        .map_err(|e| Error::Cookies(e.to_string()))?;
    store
        .insert_raw(&raw.build(), &url)
        .map_err(|e| Error::Cookies(e.to_string()))?;
    Ok(())
}

/// `$XDG_STATE_HOME/megaft` or `~/.local/state/megaft`.
#[must_use]
pub fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from(".local/state"))
        .join("megaft")
}

/// Writes `data` to `path` atomically, creating the parent with 0700 and the file with 0600.
pub fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
