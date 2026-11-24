//! evalfinder's UI-independent core, shared by the GPUI app and the ratatui TUI:
//! the three screens (login → setup → run) as plain state, their validation, and the two
//! backends that do the work (in-process, or in `megaftd` over D-Bus).

pub mod backend;
pub mod model;

pub use backend::{Backend, BackendError, DaemonBackend, JobHandle, JobUpdate, LocalBackend, connect};
pub use model::{LoginForm, RunView, SetupForm};
