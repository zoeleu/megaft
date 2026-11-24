//! Shared daemon state: the intra session, the in-memory password and the booking jobs.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::Instant,
};

use futures::future::BoxFuture;
use jiff::Timestamp;
use megaft_browser::{BrowserRefresher, Credentials};
use megaft_core::{Booked, IntraWeb, JobSpec, Session, session};
use megaft_proto::{session_state, state};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// A job as saved to `jobs.json` (no secrets).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: u32,
    pub spec: JobSpec,
    pub state: String,
    pub booked: Vec<Booked>,
    pub last_log: String,
    pub created: Timestamp,
}

#[derive(Debug)]
pub struct JobEntry {
    pub record: JobRecord,
    pub cancel: CancellationToken,
}

#[derive(Debug, Clone)]
struct Status {
    state: String,
    who: String,
}

#[derive(Debug)]
pub struct Daemon {
    session_path: PathBuf,
    jobs_path: PathBuf,
    session: tokio::sync::Mutex<Session>,
    /// Login + password from `Login()`, kept in memory only, used to renew the session.
    credentials: Mutex<Option<Credentials>>,
    status: Mutex<Status>,
    pub jobs: tokio::sync::Mutex<BTreeMap<u32, JobEntry>>,
    next_id: AtomicU32,
    last_activity: Mutex<Instant>,
}

/// Lock a std mutex, recovering from poisoning (a panicked holder can't leave these inconsistent).
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Daemon {
    pub fn new() -> anyhow::Result<Arc<Self>> {
        Self::with_paths(Session::default_path(), session::state_dir().join("jobs.json"))
    }

    pub fn with_paths(session_path: PathBuf, jobs_path: PathBuf) -> anyhow::Result<Arc<Self>> {
        let saved = Session::load(&session_path)?;
        let state = if saved.is_some() {
            session_state::EXPIRED
        } else {
            session_state::LOGGED_OUT
        };
        let jobs: Vec<JobRecord> = match std::fs::read(&jobs_path) {
            Ok(data) => serde_json::from_slice(&data).unwrap_or_else(|e| {
                tracing::warn!("ignoring unreadable {}: {e}", jobs_path.display());
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        let next = jobs.iter().map(|j| j.id).max().unwrap_or(0).saturating_add(1);
        let jobs = jobs
            .into_iter()
            .map(|r| {
                (
                    r.id,
                    JobEntry {
                        record: r,
                        cancel: CancellationToken::new(),
                    },
                )
            })
            .collect();
        Ok(Arc::new(Self {
            session_path,
            jobs_path,
            session: tokio::sync::Mutex::new(saved.unwrap_or_else(Session::empty)),
            credentials: Mutex::new(None),
            status: Mutex::new(Status {
                state: state.to_owned(),
                who: String::new(),
            }),
            jobs: tokio::sync::Mutex::new(jobs),
            next_id: AtomicU32::new(next),
            last_activity: Mutex::new(Instant::now()),
        }))
    }

    pub fn touch(&self) {
        *lock(&self.last_activity) = Instant::now();
    }

    pub fn idle_for(&self) -> std::time::Duration {
        lock(&self.last_activity).elapsed()
    }

    pub fn next_job_id(&self) -> u32 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn state(&self) -> String {
        lock(&self.status).state.clone()
    }

    pub fn who(&self) -> String {
        lock(&self.status).who.clone()
    }

    pub fn set_status(&self, state: &str, who: Option<&str>) {
        let mut s = lock(&self.status);
        state.clone_into(&mut s.state);
        if let Some(w) = who {
            w.clone_into(&mut s.who);
        }
    }

    pub fn set_credentials(&self, creds: Option<Credentials>) {
        *lock(&self.credentials) = creds;
    }

    fn refresher(&self, allow_visible: bool) -> BrowserRefresher {
        BrowserRefresher {
            session_path: self.session_path.clone(),
            credentials: lock(&self.credentials).clone(),
            allow_visible,
        }
    }

    /// A client for the current session (not checked).
    pub async fn intra(&self) -> megaft_core::Result<IntraWeb> {
        IntraWeb::new(self.session.lock().await.clone())
    }

    /// Logs in with Chrome (saved session first, then the in-memory password) and saves the session.
    pub async fn renew(&self, opts: &megaft_browser::LoginOptions) -> anyhow::Result<String> {
        self.set_status(session_state::LOGGING_IN, None);
        let mut current = self.session.lock().await;
        let creds = lock(&self.credentials).clone();
        let result = megaft_browser::login(&current.export()?, creds.as_ref(), opts).await;
        match result {
            Ok(logged) => {
                current.replace(logged.cookies, logged.user_agent)?;
                current.save(&self.session_path)?;
                self.set_status(session_state::READY, Some(&logged.login));
                Ok(logged.login)
            }
            Err(e) => {
                self.set_status(session_state::EXPIRED, None);
                Err(e.into())
            }
        }
    }

    /// Checks the saved session; renews it headlessly if it expired.
    pub async fn ensure_session(&self) -> anyhow::Result<String> {
        if let Some(who) = self.intra().await?.whoami().await? {
            self.set_status(session_state::READY, Some(&who));
            return Ok(who);
        }
        self.renew(&megaft_browser::LoginOptions::headless()).await
    }

    /// Forgets the password and deletes the saved session.
    pub async fn logout(&self) -> anyhow::Result<()> {
        self.set_credentials(None);
        *self.session.lock().await = Session::empty();
        match std::fs::remove_file(&self.session_path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        self.set_status(session_state::LOGGED_OUT, Some(""));
        Ok(())
    }

    /// Saves all job records.
    pub async fn persist_jobs(&self) {
        let records: Vec<JobRecord> = self
            .jobs
            .lock()
            .await
            .values()
            .map(|j| j.record.clone())
            .collect();
        let result = serde_json::to_vec_pretty(&records)
            .map_err(megaft_core::Error::from)
            .and_then(|data| session::write_private(&self.jobs_path, &data));
        if let Err(e) = result {
            tracing::error!("saving {}: {e}", self.jobs_path.display());
        }
    }

    pub async fn running_jobs(&self) -> usize {
        self.jobs
            .lock()
            .await
            .values()
            .filter(|j| j.record.state == state::RUNNING)
            .count()
    }
}

/// Renews the session for running jobs (headless only: a background service must not pop up windows).
#[derive(Debug, Clone)]
pub struct DaemonRefresher(pub Arc<Daemon>);

impl megaft_core::Refresher for DaemonRefresher {
    fn refresh<'a>(&'a self, _why: &'a megaft_core::Error) -> BoxFuture<'a, megaft_core::Result<IntraWeb>> {
        Box::pin(async move {
            let refresher = self.0.refresher(false);
            let current = self.0.session.lock().await.clone();
            match refresher.renew(&current).await {
                Ok((session, who)) => {
                    *self.0.session.lock().await = session.clone();
                    self.0.set_status(session_state::READY, Some(&who));
                    IntraWeb::new(session)
                }
                Err(e) => {
                    self.0.set_status(session_state::EXPIRED, None);
                    Err(megaft_core::Error::Unexpected(format!(
                        "session expired and headless login failed: {e}"
                    )))
                }
            }
        })
    }
}
