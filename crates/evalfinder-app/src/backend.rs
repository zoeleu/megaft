//! Where the work happens: in-process ([`LocalBackend`]) or in `megaftd` ([`DaemonBackend`]).
//!
//! Every method needs a tokio runtime (reqwest, zbus's tokio backend).

use std::{path::PathBuf, sync::Arc};

use futures::{StreamExt as _, future::BoxFuture};
use jiff::Timestamp;
use megaft_browser::{BrowserRefresher, Credentials, JobRefresher, LoginOptions};
use megaft_core::{Booked, IntraWeb, JobEvent, JobOutcome, JobSpec, Project, Session};
use megaft_proto::{EvaluationsProxy, JobProxy, SessionProxy, state};
use secrecy::SecretString;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Errors shown to the user.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("{0}")]
    Core(#[from] megaft_core::Error),
    #[error("{0}")]
    Browser(#[from] megaft_browser::BrowserError),
    #[error("{0}")]
    Refresh(#[from] megaft_browser::RefreshError),
    #[error("megaftd: {0}")]
    Daemon(#[from] zbus::Error),
    #[error("{0}")]
    Other(String),
}

type Result<T, E = BackendError> = std::result::Result<T, E>;

/// Progress of a running job, as seen by a UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobUpdate {
    Log(Timestamp, String),
    Booked(Booked),
    /// The job stopped; `state` is one of [`megaft_proto::state`].
    Finished {
        state: String,
        summary: String,
    },
}

/// A started job: its updates, and a way to cancel it.
#[derive(Debug)]
pub struct JobHandle {
    pub updates: mpsc::UnboundedReceiver<JobUpdate>,
    cancel: Cancel,
}

#[derive(Debug, Clone)]
enum Cancel {
    Local(CancellationToken),
    Daemon(JobProxy<'static>),
}

impl JobHandle {
    /// Asks the job to stop. A `Finished` update follows.
    pub async fn cancel(&self) -> Result<()> {
        match &self.cancel {
            Cancel::Local(t) => {
                t.cancel();
                Ok(())
            }
            Cancel::Daemon(p) => Ok(p.cancel().await?),
        }
    }

    /// A cloneable canceller, for UIs that hand the receiver to another task.
    #[must_use]
    pub fn canceller(&self) -> Canceller {
        Canceller(self.cancel.clone())
    }
}

/// Cancels a job (see [`JobHandle::canceller`]).
#[derive(Debug, Clone)]
pub struct Canceller(Cancel);

impl Canceller {
    pub async fn cancel(&self) -> Result<()> {
        match &self.0 {
            Cancel::Local(t) => {
                t.cancel();
                Ok(())
            }
            Cancel::Daemon(p) => Ok(p.cancel().await?),
        }
    }
}

/// What the screens need from a backend.
pub trait Backend: Send + Sync + std::fmt::Debug {
    /// "this app" or "megaftd (keeps running after you close the app)".
    fn describe(&self) -> &'static str;
    /// Checks the saved session (renewing it headlessly if needed). Returns the login.
    fn use_saved_session(&self) -> BoxFuture<'_, Result<String>>;
    /// Logs in with a password (checked against the intra). Returns the login.
    fn login(&self, login: String, password: SecretString) -> BoxFuture<'_, Result<String>>;
    /// Opens a visible Chrome window to log in by hand.
    fn login_interactive(&self) -> BoxFuture<'_, Result<String>>;
    fn open_projects(&self) -> BoxFuture<'_, Result<Vec<Project>>>;
    fn start_job(&self, spec: JobSpec) -> BoxFuture<'_, Result<JobHandle>>;
}

/// Uses megaftd if it's installed (D-Bus activatable or running), unless `force_local`.
pub async fn connect(force_local: bool) -> Arc<dyn Backend> {
    if !force_local {
        match DaemonBackend::connect().await {
            Ok(Some(d)) => return Arc::new(d),
            Ok(None) => tracing::info!("megaftd not installed; running jobs in-process"),
            Err(e) => tracing::warn!("megaftd unreachable ({e}); running jobs in-process"),
        }
    }
    Arc::new(LocalBackend::new(Session::default_path()))
}

// ───────────────────────────── Local ─────────────────────────────

/// Runs everything in this process. The password stays in this process's memory.
#[derive(Debug)]
pub struct LocalBackend {
    session_path: PathBuf,
    session: tokio::sync::Mutex<Session>,
    credentials: std::sync::Mutex<Option<Credentials>>,
}

impl LocalBackend {
    #[must_use]
    pub fn new(session_path: PathBuf) -> Self {
        let session = Session::load(&session_path)
            .ok()
            .flatten()
            .unwrap_or_else(Session::empty);
        Self {
            session_path,
            session: tokio::sync::Mutex::new(session),
            credentials: std::sync::Mutex::new(None),
        }
    }

    fn creds(&self) -> Option<Credentials> {
        self.credentials
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    async fn browser_login(&self, opts: &LoginOptions) -> Result<String> {
        let mut session = self.session.lock().await;
        let creds = self.creds();
        let logged = megaft_browser::login(&session.export()?, creds.as_ref(), opts).await?;
        session.replace(logged.cookies, logged.user_agent)?;
        session.save(&self.session_path)?;
        Ok(logged.login)
    }

    async fn intra(&self) -> Result<IntraWeb> {
        Ok(IntraWeb::new(self.session.lock().await.clone())?)
    }
}

impl Backend for LocalBackend {
    fn describe(&self) -> &'static str {
        "this app (stops when you close it)"
    }

    fn use_saved_session(&self) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            if let Some(who) = self.intra().await?.whoami().await? {
                return Ok(who);
            }
            self.browser_login(&LoginOptions::headless()).await
        })
    }

    fn login(&self, login: String, password: SecretString) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            let previous = self.creds();
            *self
                .credentials
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Credentials {
                login: login.clone(),
                password,
            });
            let mut opts = LoginOptions::headless();
            opts.fresh = true;
            match self.browser_login(&opts).await {
                Ok(who) if who == login => Ok(who),
                other => {
                    *self
                        .credentials
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = previous;
                    match other {
                        Ok(who) => Err(BackendError::Other(format!("logged in as {who}, not {login}"))),
                        Err(e) => Err(e),
                    }
                }
            }
        })
    }

    fn login_interactive(&self) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move { self.browser_login(&LoginOptions::visible()).await })
    }

    fn open_projects(&self) -> BoxFuture<'_, Result<Vec<Project>>> {
        Box::pin(async move { Ok(self.intra().await?.open_projects().await?) })
    }

    fn start_job(&self, spec: JobSpec) -> BoxFuture<'_, Result<JobHandle>> {
        Box::pin(async move {
            let intra = self.intra().await?;
            let refresher = JobRefresher {
                browser: BrowserRefresher {
                    session_path: self.session_path.clone(),
                    credentials: self.creds(),
                    allow_visible: true, // an app the user is looking at may open a window
                },
                session: tokio::sync::Mutex::new(intra.session().clone()),
            };
            let (tx, rx) = mpsc::unbounded_channel();
            let cancel = CancellationToken::new();
            let token = cancel.clone();
            tokio::spawn(async move {
                let (events_tx, mut events_rx) = mpsc::unbounded_channel();
                let forward_tx = tx.clone();
                let forward = tokio::spawn(async move {
                    while let Some(ev) = events_rx.recv().await {
                        let update = match ev {
                            JobEvent::Log(l) => JobUpdate::Log(Timestamp::now(), l),
                            JobEvent::Booked(b) => JobUpdate::Booked(b),
                        };
                        if forward_tx.send(update).is_err() {
                            break;
                        }
                    }
                });
                let result = megaft_core::job::run(intra, &refresher, &spec, &token, &events_tx).await;
                drop(events_tx);
                if let Err(e) = forward.await {
                    tracing::warn!("job event forwarder: {e}");
                }
                let finished = finished_update(&spec, result);
                if tx.send(finished).is_err() {
                    tracing::debug!("job finished after the UI went away");
                }
            });
            Ok(JobHandle {
                updates: rx,
                cancel: Cancel::Local(cancel),
            })
        })
    }
}

fn finished_update(spec: &JobSpec, result: megaft_core::Result<(JobOutcome, Vec<Booked>)>) -> JobUpdate {
    let verb = if spec.dry_run { "found" } else { "booked" };
    let (state, summary) = match result {
        Ok((JobOutcome::Done, b)) => (state::DONE, format!("Done: {}/{} {verb}.", b.len(), spec.count)),
        Ok((JobOutcome::Deadline, b)) => (
            state::DEADLINE,
            format!("Deadline reached: {}/{} {verb}.", b.len(), spec.count),
        ),
        Ok((JobOutcome::Cancelled, b)) => (
            state::CANCELLED,
            format!("Stopped: {}/{} {verb}.", b.len(), spec.count),
        ),
        Err(e) => (state::ERROR, format!("Error: {e}")),
    };
    JobUpdate::Finished {
        state: state.to_owned(),
        summary,
    }
}

// ───────────────────────────── Daemon ────────────────────────────

/// Hands the work to megaftd over D-Bus. Jobs keep running after the app closes.
#[derive(Debug, Clone)]
pub struct DaemonBackend {
    conn: zbus::Connection,
}

impl DaemonBackend {
    /// `Ok(None)` if megaftd isn't running and can't be activated.
    pub async fn connect() -> Result<Option<Self>> {
        let conn = zbus::Connection::session().await?;
        let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
        let name = zbus::names::BusName::try_from(megaft_proto::BUS_NAME)?;
        let running = dbus.name_has_owner(name.clone()).await.unwrap_or(false);
        let activatable = dbus
            .list_activatable_names()
            .await
            .map(|names| names.iter().any(|n| n.as_str() == megaft_proto::BUS_NAME))
            .unwrap_or(false);
        Ok((running || activatable).then_some(Self { conn }))
    }

    async fn session(&self) -> Result<SessionProxy<'static>> {
        Ok(SessionProxy::new(&self.conn).await?)
    }

    async fn evaluations(&self) -> Result<EvaluationsProxy<'static>> {
        Ok(EvaluationsProxy::new(&self.conn).await?)
    }
}

/// D-Bus calls that run Chrome can take a while.
fn slow<T>(r: zbus::Result<T>) -> Result<T> {
    r.map_err(|e| match e {
        zbus::Error::MethodError(_, Some(msg), _) => BackendError::Other(msg),
        e => BackendError::Daemon(e),
    })
}

impl Backend for DaemonBackend {
    fn describe(&self) -> &'static str {
        "megaftd (keeps running after you close the app)"
    }

    fn use_saved_session(&self) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move { slow(self.session().await?.use_saved_session().await) })
    }

    fn login(&self, login: String, password: SecretString) -> BoxFuture<'_, Result<String>> {
        use secrecy::ExposeSecret as _;
        Box::pin(async move {
            slow(
                self.session()
                    .await?
                    .login(&login, password.expose_secret())
                    .await,
            )
        })
    }

    fn login_interactive(&self) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move { slow(self.session().await?.login_interactive().await) })
    }

    fn open_projects(&self) -> BoxFuture<'_, Result<Vec<Project>>> {
        Box::pin(async move {
            let raw = slow(self.evaluations().await?.open_projects().await)?;
            Ok(raw
                .into_iter()
                .map(|(slug, name, status)| Project { slug, name, status })
                .collect())
        })
    }

    fn start_job(&self, spec: JobSpec) -> BoxFuture<'_, Result<JobHandle>> {
        Box::pin(async move {
            let path = slow(
                self.evaluations()
                    .await?
                    .create_job(
                        &spec.project,
                        spec.count,
                        spec.deadline.as_second(),
                        spec.buffer_minutes,
                        spec.dry_run,
                    )
                    .await,
            )?;
            let job = JobProxy::builder(&self.conn).path(path).build().await?;
            let (tx, rx) = mpsc::unbounded_channel();
            let mut logs = job.receive_log().await?;
            let mut booked = job.receive_booked().await?;
            let mut states = job.receive_state_changed().await;
            let watcher = job.clone();
            let dry_run = spec.dry_run;
            tokio::spawn(async move {
                // Catch up on anything that happened before we subscribed (fast dry runs).
                let mut seen = std::collections::BTreeSet::new();
                for (s, e) in watcher.booked().await.unwrap_or_default() {
                    if let (Ok(start), Ok(end)) = (Timestamp::from_second(s), Timestamp::from_second(e))
                        && seen.insert(s)
                        && tx
                            .send(JobUpdate::Booked(Booked { start, end, dry_run }))
                            .is_err()
                    {
                        return;
                    }
                }
                if let Ok(s) = watcher.state().await
                    && s != state::RUNNING
                {
                    let summary = watcher.last_log().await.unwrap_or_default();
                    if tx.send(JobUpdate::Finished { state: s, summary }).is_err() {
                        tracing::debug!("UI went away");
                    }
                    return;
                }
                loop {
                    let update = tokio::select! {
                        Some(sig) = logs.next() => match sig.args() {
                            Ok(a) => JobUpdate::Log(Timestamp::from_second(a.time).unwrap_or_else(|_| Timestamp::now()), a.line),
                            Err(e) => { tracing::debug!("Log signal: {e}"); continue }
                        },
                        Some(sig) = booked.next() => match sig.args() {
                            Ok(a) => match (Timestamp::from_second(a.start), Timestamp::from_second(a.end)) {
                                (Ok(start), Ok(end)) if seen.insert(a.start) => JobUpdate::Booked(Booked { start, end, dry_run }),
                                _ => continue,
                            },
                            Err(e) => { tracing::debug!("Booked signal: {e}"); continue }
                        },
                        Some(change) = states.next() => {
                            let Ok(s) = change.get().await else { continue };
                            if s == state::RUNNING { continue }
                            let summary = watcher.last_log().await.unwrap_or_default();
                            JobUpdate::Finished { state: s, summary }
                        },
                        else => break,
                    };
                    let done = matches!(update, JobUpdate::Finished { .. });
                    if tx.send(update).is_err() || done {
                        break;
                    }
                }
            });
            Ok(JobHandle {
                updates: rx,
                cancel: Cancel::Daemon(job),
            })
        })
    }
}
