//! megaftd: the megaft background service. See `PLAN.md` §4 and `megaft-proto/megaft1.xml`.

mod daemon;
mod ifaces;

use std::time::Duration;

use megaft_proto::{BUS_NAME, ROOT_PATH, state};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};

use crate::{
    daemon::Daemon,
    ifaces::{EvaluationsIface, SessionIface, register_job},
};

/// Exit after this long with no running jobs and no D-Bus calls (D-Bus/systemd start us again).
const IDLE_EXIT: Duration = Duration::from_secs(10 * 60);
/// Finished jobs are forgotten after a week.
const KEEP_FINISHED: jiff::SignedDuration = jiff::SignedDuration::from_hours(24 * 7);

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let registry = tracing_subscriber::registry().with(filter);
    // Under systemd use the native journal protocol (keeps log levels); otherwise plain stderr.
    if std::env::var_os("JOURNAL_STREAM").is_some()
        && let Ok(journald) = tracing_journald::layer()
    {
        registry
            .with(journald.with_syslog_identifier("megaftd".into()))
            .init();
    } else {
        registry.with(tracing_subscriber::fmt::layer()).init();
    }
}

fn notify_status(msg: &str) {
    if let Err(e) = sd_notify::notify(&[sd_notify::NotifyState::Status(msg)]) {
        tracing::debug!("sd_notify: {e}");
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging();
    let daemon = Daemon::new()?;

    let conn = zbus::connection::Builder::session()
        .serve_at(ROOT_PATH, SessionIface(daemon.clone()))
        .serve_at(ROOT_PATH, EvaluationsIface(daemon.clone()))
        .build()
        .await?;

    // Re-export saved jobs; resume the ones that were running and whose deadline hasn't passed.
    let saved: Vec<_> = std::mem::take(&mut *daemon.jobs.lock().await)
        .into_values()
        .map(|j| j.record)
        .collect();
    let now = jiff::Timestamp::now();
    for mut record in saved {
        if record.state != state::RUNNING && record.created + KEEP_FINISHED < now {
            continue;
        }
        if record.state == state::RUNNING && record.spec.deadline <= now {
            state::DEADLINE.clone_into(&mut record.state);
            "deadline passed while megaftd was stopped".clone_into(&mut record.last_log);
        }
        if record.state == state::RUNNING {
            tracing::info!("resuming job {} ({})", record.id, record.spec.project);
        }
        if let Err(e) = register_job(&conn, &daemon, record).await {
            tracing::error!("restoring job: {e}");
        }
    }

    // Take the name last, so D-Bus activation only sees us once every object is exported.
    conn.request_name(BUS_NAME).await?;
    tracing::info!("serving {BUS_NAME} at {ROOT_PATH}");
    notify_status("ready");

    let mut tick = tokio::time::interval(Duration::from_secs(30));
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let running = daemon.running_jobs().await;
                let who = daemon.who();
                notify_status(&match running {
                    0 if who.is_empty() => "idle, not logged in".to_owned(),
                    0 => format!("idle, logged in as {who}"),
                    1 => "1 job running".to_owned(),
                    n => format!("{n} jobs running"),
                });
                if running == 0 && daemon.idle_for() > IDLE_EXIT {
                    tracing::info!("idle for {} min, exiting", IDLE_EXIT.as_secs() / 60);
                    break;
                }
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = term.recv() => break,
        }
    }
    // Running jobs stay "running" in jobs.json and resume on the next start.
    notify_status("stopping");
    Ok(())
}

#[cfg(test)]
mod tests {
    //! The daemon's real objects must match `megaft1.xml` (the GNOME extension is built from it).

    use std::collections::BTreeSet;

    use megaft_core::JobSpec;
    use zbus::fdo::IntrospectableProxy;

    use super::*;
    use crate::daemon::JobRecord;

    /// Every method/property/signal as `iface.member(signature)`, comparable across sources.
    fn members(node: &zbus_xml::Node<'_>) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for i in node.interfaces() {
            let iface = i.name().to_string();
            if iface.starts_with("org.freedesktop.DBus.") {
                continue;
            }
            for m in i.methods() {
                let sig: Vec<_> = m
                    .args()
                    .iter()
                    .map(|a| format!("{:?}:{}", a.direction(), a.ty()))
                    .collect();
                out.insert(format!("{iface}.{}({})", m.name(), sig.join(",")));
            }
            for p in i.properties() {
                out.insert(format!("{iface}.{} {} {:?}", p.name(), p.ty(), p.access()));
            }
            for s in i.signals() {
                let sig: Vec<_> = s.args().iter().map(|a| a.ty().to_string()).collect();
                out.insert(format!("{iface}!{}({})", s.name(), sig.join(",")));
            }
        }
        out
    }

    #[tokio::test]
    async fn objects_match_introspection_xml() {
        let dir = tempfile::tempdir().unwrap();
        let daemon =
            Daemon::with_paths(dir.path().join("session.json"), dir.path().join("jobs.json")).unwrap();
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let guid = zbus::Guid::generate();
        let (server, client) = futures::try_join!(
            zbus::connection::Builder::unix_stream(a)
                .server(guid)
                .p2p()
                .serve_at(ROOT_PATH, SessionIface(daemon.clone()))
                .serve_at(ROOT_PATH, EvaluationsIface(daemon.clone()))
                .build(),
            zbus::connection::Builder::unix_stream(b).p2p().build(),
        )
        .unwrap();
        // A finished job, so its object is exported without starting a booking loop.
        let mut spec = JobSpec::new("codexion".into(), 1, jiff::Timestamp::now());
        spec.dry_run = true;
        let record = JobRecord {
            id: 1,
            spec,
            state: state::DONE.into(),
            booked: Vec::new(),
            last_log: String::new(),
            created: jiff::Timestamp::now(),
        };
        let job_path = register_job(&server, &daemon, record).await.unwrap();

        let mut live = BTreeSet::new();
        for path in [ROOT_PATH.to_owned(), job_path.to_string()] {
            let xml = IntrospectableProxy::builder(&client)
                .destination(BUS_NAME)
                .path(path)
                .build()
                .await
                .unwrap()
                .introspect()
                .await
                .unwrap();
            live.extend(members(&zbus_xml::Node::from_reader(xml.as_bytes()).unwrap()));
        }
        let spec = members(&megaft_proto::introspection().unwrap());
        assert_eq!(live, spec, "megaftd's objects and megaft1.xml differ");
    }
}
