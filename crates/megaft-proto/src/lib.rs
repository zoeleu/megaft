//! The megaftd D-Bus API, version 1. `megaft1.xml` is the source of truth; the proxies below
//! mirror it and megaftd's tests check its objects against it.

use zbus::{OwnedObjectPath, proxy};

/// Well-known bus name (session bus).
pub const BUS_NAME: &str = "br.dev.zoe.MegaFt1";
/// Object path of the Session and Evaluations interfaces.
pub const ROOT_PATH: &str = "/br/dev/zoe/MegaFt1";
/// Job objects live at `JOBS_PATH/<n>`.
pub const JOBS_PATH: &str = "/br/dev/zoe/MegaFt1/jobs";

pub const SESSION_IFACE: &str = "br.dev.zoe.MegaFt1.Session";
pub const EVALUATIONS_IFACE: &str = "br.dev.zoe.MegaFt1.Evaluations";
pub const JOB_IFACE: &str = "br.dev.zoe.MegaFt1.Job";

/// The introspection XML (also copied into the GNOME extension).
pub const INTROSPECTION_XML: &str = include_str!("../megaft1.xml");

/// Parses [`INTROSPECTION_XML`].
pub fn introspection() -> zbus_xml::Result<zbus_xml::Node<'static>> {
    zbus_xml::Node::from_reader(INTROSPECTION_XML.as_bytes())
}

/// Job states (`Job.State`).
pub mod state {
    pub const RUNNING: &str = "running";
    pub const DONE: &str = "done";
    pub const DEADLINE: &str = "deadline";
    pub const CANCELLED: &str = "cancelled";
    pub const ERROR: &str = "error";
}

/// Session states (`Session.State`).
pub mod session_state {
    pub const LOGGED_OUT: &str = "logged-out";
    pub const LOGGING_IN: &str = "logging-in";
    pub const READY: &str = "ready";
    pub const EXPIRED: &str = "expired";
}

#[proxy(
    interface = "br.dev.zoe.MegaFt1.Session",
    default_service = "br.dev.zoe.MegaFt1",
    default_path = "/br/dev/zoe/MegaFt1"
)]
pub trait Session {
    fn login(&self, login: &str, password: &str) -> zbus::Result<String>;
    fn login_interactive(&self) -> zbus::Result<String>;
    fn use_saved_session(&self) -> zbus::Result<String>;
    fn logout(&self) -> zbus::Result<()>;
    #[zbus(property)]
    fn state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn who(&self) -> zbus::Result<String>;
}

#[proxy(
    interface = "br.dev.zoe.MegaFt1.Evaluations",
    default_service = "br.dev.zoe.MegaFt1",
    default_path = "/br/dev/zoe/MegaFt1"
)]
pub trait Evaluations {
    fn open_projects(&self) -> zbus::Result<Vec<(String, String, String)>>;
    fn upcoming(&self, slug: &str) -> zbus::Result<Vec<i64>>;
    fn create_job(
        &self,
        slug: &str,
        count: u32,
        deadline: i64,
        buffer_minutes: u32,
        dry_run: bool,
    ) -> zbus::Result<OwnedObjectPath>;
    fn list_jobs(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
    #[zbus(signal)]
    fn job_added(&self, job: OwnedObjectPath) -> zbus::Result<()>;
    #[zbus(signal)]
    fn job_removed(&self, job: OwnedObjectPath) -> zbus::Result<()>;
}

#[proxy(interface = "br.dev.zoe.MegaFt1.Job", default_service = "br.dev.zoe.MegaFt1")]
pub trait Job {
    fn cancel(&self) -> zbus::Result<()>;
    fn dismiss(&self) -> zbus::Result<()>;
    #[zbus(property)]
    fn state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn project(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn count(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn deadline(&self) -> zbus::Result<i64>;
    #[zbus(property)]
    fn dry_run(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn booked(&self) -> zbus::Result<Vec<(i64, i64)>>;
    #[zbus(property)]
    fn last_log(&self) -> zbus::Result<String>;
    #[zbus(signal)]
    fn log(&self, time: i64, line: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn booked(&self, start: i64, end: i64) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_parses_and_has_three_interfaces() {
        let node = introspection().unwrap();
        let names: Vec<_> = node.interfaces().iter().map(|i| i.name().to_string()).collect();
        assert_eq!(names, [SESSION_IFACE, EVALUATIONS_IFACE, JOB_IFACE]);
    }

    #[test]
    fn extension_copy_is_identical() {
        let ext = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../extension/ftgnome@zoe.dev.br/dbus/megaft1.xml");
        let copy = std::fs::read_to_string(&ext).unwrap();
        assert_eq!(
            copy, INTROSPECTION_XML,
            "run `make ext-sync-xml` after changing megaft1.xml"
        );
    }
}
