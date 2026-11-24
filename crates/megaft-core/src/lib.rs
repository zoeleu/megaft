//! 42 intra web client and evaluation-slot booking logic (Rust port of evalfinder.py).
//!
//! The public API v2 can't book evaluations as a student, so this talks to the same endpoints the
//! intra web client uses (see `PLAN.md` §1):
//!
//! * `GET  /projects/<slug>/slots?team_id=` — HTML: team id, duration, URLs, CSRF token
//! * `GET  /projects/<slug>/slots.json?team_id=&start=&end=` — open blocks of 15-minute slots
//! * `POST /projects/<slug>/slots/<ids>.json?team_id=` (`_method=put`) — book
//! * `GET  /<slug>/mine` — evaluations already booked
//! * `GET  /` — open teams and their status
#![warn(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss
)]

pub mod error;
pub mod intra;
pub mod job;
pub mod parse;
pub mod session;
pub mod slots;

pub use error::{Error, Result};
pub use intra::{BookResult, BookingPage, DateRange, IntraWeb, Project};
pub use job::{Booked, JobEvent, JobOutcome, JobSpec, NoRefresh, Refresher};
pub use session::{BrowserCookie, Session};
pub use slots::{Candidate, SLOT_MINUTES, SlotBlock, Span};
