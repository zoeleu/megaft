//! The D-Bus objects (see `megaft-proto/megaft1.xml`).

use std::sync::Arc;

use jiff::Timestamp;
use megaft_browser::{Credentials, LoginOptions};
use megaft_core::{JobEvent, JobOutcome, JobSpec};
use megaft_proto::{JOBS_PATH, state};
use secrecy::SecretString;
use tokio_util::sync::CancellationToken;
use zbus::{
    Connection, ObjectServer, OwnedObjectPath,
    fdo::{self, Error as FdoError},
    interface,
    object_server::SignalEmitter,
};

use crate::daemon::{Daemon, DaemonRefresher, JobEntry, JobRecord};

fn failed(e: impl std::fmt::Display) -> FdoError {
    FdoError::Failed(e.to_string())
}

pub fn job_path(id: u32) -> fdo::Result<OwnedObjectPath> {
    OwnedObjectPath::try_from(format!("{JOBS_PATH}/{id}")).map_err(failed)
}

// ───────────────────────────── Session ─────────────────────────────

#[derive(Debug)]
pub struct SessionIface(pub Arc<Daemon>);

impl SessionIface {
    async fn changed(&self, emitter: &SignalEmitter<'_>) {
        if let Err(e) = self.state_changed(emitter).await {
            tracing::debug!("State changed signal: {e}");
        }
        if let Err(e) = self.who_changed(emitter).await {
            tracing::debug!("Who changed signal: {e}");
        }
    }
}

#[interface(name = "br.dev.zoe.MegaFt1.Session")]
impl SessionIface {
    async fn login(
        &self,
        login: String,
        password: String,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<String> {
        self.0.touch();
        let creds = Credentials {
            login: login.clone(),
            password: SecretString::from(password),
        };
        let previous = self.0.who();
        self.0.set_credentials(Some(creds));
        let mut opts = LoginOptions::headless();
        opts.fresh = true; // really check the password
        let result = self.0.renew(&opts).await;
        self.changed(&emitter).await;
        match result {
            Ok(who) if who == login => Ok(who),
            Ok(who) => {
                self.0.set_credentials(None);
                Err(FdoError::Failed(format!(
                    "logged in as {who}, not {login} (previously {previous})"
                )))
            }
            Err(e) => {
                self.0.set_credentials(None);
                Err(failed(e))
            }
        }
    }

    async fn login_interactive(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<String> {
        self.0.touch();
        let result = self.0.renew(&LoginOptions::visible()).await;
        self.changed(&emitter).await;
        result.map_err(failed)
    }

    async fn use_saved_session(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<String> {
        self.0.touch();
        let result = self.0.ensure_session().await;
        self.changed(&emitter).await;
        result.map_err(failed)
    }

    async fn logout(&self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) -> fdo::Result<()> {
        self.0.touch();
        let result = self.0.logout().await;
        self.changed(&emitter).await;
        result.map_err(failed)
    }

    #[zbus(property)]
    fn state(&self) -> String {
        self.0.state()
    }

    #[zbus(property)]
    fn who(&self) -> String {
        self.0.who()
    }
}

// ─────────────────────────── Evaluations ───────────────────────────

#[derive(Debug)]
pub struct EvaluationsIface(pub Arc<Daemon>);

#[interface(name = "br.dev.zoe.MegaFt1.Evaluations")]
impl EvaluationsIface {
    async fn open_projects(&self) -> fdo::Result<Vec<(String, String, String)>> {
        self.0.touch();
        self.0.ensure_session().await.map_err(failed)?;
        let intra = self.0.intra().await.map_err(failed)?;
        let projects = intra.open_projects().await.map_err(failed)?;
        Ok(projects.into_iter().map(|p| (p.slug, p.name, p.status)).collect())
    }

    async fn upcoming(&self, slug: String) -> fdo::Result<Vec<i64>> {
        self.0.touch();
        self.0.ensure_session().await.map_err(failed)?;
        let intra = self.0.intra().await.map_err(failed)?;
        let starts = intra.upcoming_evaluations(&slug).await.map_err(failed)?;
        Ok(starts.iter().map(|t| t.as_second()).collect())
    }

    async fn create_job(
        &self,
        slug: String,
        count: u32,
        deadline: i64,
        buffer_minutes: u32,
        dry_run: bool,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<OwnedObjectPath> {
        self.0.touch();
        if count == 0 {
            return Err(FdoError::InvalidArgs("count must be at least 1".into()));
        }
        let deadline = Timestamp::from_second(deadline).map_err(|e| FdoError::InvalidArgs(e.to_string()))?;
        if deadline <= Timestamp::now() {
            return Err(FdoError::InvalidArgs("the deadline is in the past".into()));
        }
        {
            let jobs = self.0.jobs.lock().await;
            if jobs
                .values()
                .any(|j| j.record.state == state::RUNNING && j.record.spec.project == slug)
            {
                return Err(FdoError::Failed(format!("a job is already running for {slug}")));
            }
        }
        let mut spec = JobSpec::new(slug, count, deadline);
        spec.buffer_minutes = buffer_minutes;
        spec.dry_run = dry_run;
        let id = self.0.next_job_id();
        let record = JobRecord {
            id,
            spec,
            state: state::RUNNING.to_owned(),
            booked: Vec::new(),
            last_log: "starting".to_owned(),
            created: Timestamp::now(),
        };
        let path = register_job(conn, &self.0, record).await?;
        let emitter = SignalEmitter::new(conn, megaft_proto::ROOT_PATH).map_err(failed)?;
        if let Err(e) = Self::job_added(&emitter, path.clone()).await {
            tracing::debug!("JobAdded signal: {e}");
        }
        Ok(path)
    }

    async fn list_jobs(&self) -> fdo::Result<Vec<OwnedObjectPath>> {
        self.0.touch();
        self.0.jobs.lock().await.keys().map(|id| job_path(*id)).collect()
    }

    #[zbus(signal)]
    async fn job_added(emitter: &SignalEmitter<'_>, job: OwnedObjectPath) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn job_removed(emitter: &SignalEmitter<'_>, job: OwnedObjectPath) -> zbus::Result<()>;
}

// ─────────────────────────────── Job ───────────────────────────────

#[derive(Debug)]
pub struct JobIface {
    pub id: u32,
    pub daemon: Arc<Daemon>,
}

impl JobIface {
    async fn with<T>(&self, f: impl FnOnce(&JobRecord) -> T) -> fdo::Result<T> {
        let jobs = self.daemon.jobs.lock().await;
        jobs.get(&self.id)
            .map(|j| f(&j.record))
            .ok_or_else(|| FdoError::UnknownObject("job is gone".into()))
    }
}

#[interface(name = "br.dev.zoe.MegaFt1.Job")]
impl JobIface {
    async fn cancel(&self) -> fdo::Result<()> {
        self.daemon.touch();
        let jobs = self.daemon.jobs.lock().await;
        let job = jobs
            .get(&self.id)
            .ok_or_else(|| FdoError::UnknownObject("job is gone".into()))?;
        job.cancel.cancel();
        Ok(())
    }

    async fn dismiss(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        self.daemon.touch();
        {
            let mut jobs = self.daemon.jobs.lock().await;
            match jobs.get(&self.id) {
                Some(j) if j.record.state == state::RUNNING => {
                    return Err(FdoError::Failed("cancel the job before dismissing it".into()));
                }
                Some(_) => {
                    jobs.remove(&self.id);
                }
                None => return Err(FdoError::UnknownObject("job is gone".into())),
            }
        }
        self.daemon.persist_jobs().await;
        let path = job_path(self.id)?;
        let emitter = SignalEmitter::new(conn, megaft_proto::ROOT_PATH).map_err(failed)?;
        if let Err(e) = EvaluationsIface::job_removed(&emitter, path.clone()).await {
            tracing::debug!("JobRemoved signal: {e}");
        }
        // Removing ourselves from inside our own method is fine: zbus finishes this call first.
        let server = server.clone();
        tokio::spawn(async move {
            if let Err(e) = server.remove::<Self, _>(path).await {
                tracing::debug!("removing job object: {e}");
            }
        });
        Ok(())
    }

    #[zbus(property)]
    async fn state(&self) -> String {
        self.with(|r| r.state.clone()).await.unwrap_or_default()
    }

    #[zbus(property)]
    async fn project(&self) -> String {
        self.with(|r| r.spec.project.clone()).await.unwrap_or_default()
    }

    #[zbus(property)]
    async fn count(&self) -> u32 {
        self.with(|r| r.spec.count).await.unwrap_or_default()
    }

    #[zbus(property)]
    async fn deadline(&self) -> i64 {
        self.with(|r| r.spec.deadline.as_second())
            .await
            .unwrap_or_default()
    }

    #[zbus(property)]
    async fn dry_run(&self) -> bool {
        self.with(|r| r.spec.dry_run).await.unwrap_or_default()
    }

    #[zbus(property)]
    async fn booked(&self) -> Vec<(i64, i64)> {
        self.with(|r| {
            r.booked
                .iter()
                .map(|b| (b.start.as_second(), b.end.as_second()))
                .collect()
        })
        .await
        .unwrap_or_default()
    }

    #[zbus(property)]
    async fn last_log(&self) -> String {
        self.with(|r| r.last_log.clone()).await.unwrap_or_default()
    }

    #[zbus(signal)]
    async fn log(emitter: &SignalEmitter<'_>, time: i64, line: String) -> zbus::Result<()>;

    #[zbus(signal, name = "Booked")]
    async fn booked_signal(emitter: &SignalEmitter<'_>, start: i64, end: i64) -> zbus::Result<()>;
}

/// Adds the job to the daemon, exports its object, and starts it if it's running.
pub async fn register_job(
    conn: &Connection,
    daemon: &Arc<Daemon>,
    record: JobRecord,
) -> fdo::Result<OwnedObjectPath> {
    let id = record.id;
    let running = record.state == state::RUNNING;
    let cancel = CancellationToken::new();
    daemon.jobs.lock().await.insert(
        id,
        JobEntry {
            record,
            cancel: cancel.clone(),
        },
    );
    daemon.persist_jobs().await;
    let path = job_path(id)?;
    conn.object_server()
        .at(
            path.clone(),
            JobIface {
                id,
                daemon: daemon.clone(),
            },
        )
        .await
        .map_err(failed)?;
    if running {
        tokio::spawn(run_job(conn.clone(), daemon.clone(), id, cancel));
    }
    Ok(path)
}

async fn job_ref(conn: &Connection, id: u32) -> Option<zbus::object_server::InterfaceRef<JobIface>> {
    let path = job_path(id).ok()?;
    conn.object_server()
        .interface::<_, JobIface>(path)
        .await
        .inspect_err(|e| tracing::debug!("job {id} object: {e}"))
        .ok()
}

fn log_signal_error(id: u32, r: zbus::Result<()>) {
    if let Err(e) = r {
        tracing::debug!("job {id} signal: {e}");
    }
}

async fn run_job(conn: Connection, daemon: Arc<Daemon>, id: u32, cancel: CancellationToken) {
    let Some(spec) = daemon.jobs.lock().await.get(&id).map(|j| j.record.spec.clone()) else {
        return;
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    let forward = {
        let (conn, daemon) = (conn.clone(), daemon.clone());
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                match ev {
                    JobEvent::Log(line) => {
                        if let Some(j) = daemon.jobs.lock().await.get_mut(&id) {
                            j.record.last_log.clone_from(&line);
                        }
                        if let Some(r) = job_ref(&conn, id).await {
                            let em = r.signal_emitter();
                            log_signal_error(id, JobIface::log(em, Timestamp::now().as_second(), line).await);
                            log_signal_error(id, r.get().await.last_log_changed(em).await);
                        }
                    }
                    JobEvent::Booked(b) => {
                        if let Some(j) = daemon.jobs.lock().await.get_mut(&id) {
                            j.record.booked.push(b.clone());
                        }
                        daemon.persist_jobs().await;
                        if let Some(r) = job_ref(&conn, id).await {
                            let em = r.signal_emitter();
                            let (s, e) = (b.start.as_second(), b.end.as_second());
                            log_signal_error(id, JobIface::booked_signal(em, s, e).await);
                            log_signal_error(id, r.get().await.booked_changed(em).await);
                        }
                    }
                }
            }
        })
    };

    let refresher = DaemonRefresher(daemon.clone());
    let result = match daemon.intra().await {
        Ok(intra) => megaft_core::job::run(intra, &refresher, &spec, &cancel, &tx).await,
        Err(e) => Err(e),
    };
    drop(tx);
    if let Err(e) = forward.await {
        tracing::warn!("job {id} event forwarder: {e}");
    }

    let verb = if spec.dry_run { "found" } else { "booked" };
    let (final_state, last) = match result {
        Ok((JobOutcome::Done, b)) => (state::DONE, format!("done: {}/{} {verb}", b.len(), spec.count)),
        Ok((JobOutcome::Deadline, b)) => (
            state::DEADLINE,
            format!("deadline reached: {}/{} {verb}", b.len(), spec.count),
        ),
        Ok((JobOutcome::Cancelled, b)) => (
            state::CANCELLED,
            format!("cancelled: {}/{} {verb}", b.len(), spec.count),
        ),
        Err(e) => (state::ERROR, format!("error: {e}")),
    };
    tracing::info!("job {id} ({}): {last}", spec.project);
    if let Some(j) = daemon.jobs.lock().await.get_mut(&id) {
        final_state.clone_into(&mut j.record.state);
        j.record.last_log.clone_from(&last);
    }
    daemon.persist_jobs().await;
    if let Some(r) = job_ref(&conn, id).await {
        let em = r.signal_emitter();
        let job = r.get().await;
        log_signal_error(id, job.state_changed(em).await);
        log_signal_error(id, job.last_log_changed(em).await);
    }
}
