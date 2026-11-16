//! Citroenition team sync: the part that makes a profile belong to a team hub.
//!
//! A hub profile is **not launched without the hub's lock** ([`citro_core::launch::start`] refuses it), so every
//! way of opening one (the `citro` CLI, the Local API, the app) goes through [`open`]. While the browser runs a
//! supervisor keeps the lease alive; closing uploads the cookies and profile data and hands the lock back. See
//! `docs/HUB.md` for the concepts and `docs.md` for the commands.
//!
//! * [`Hub`]: log in, pull the team's profiles, push a local one, locks, edits written through to the hub.
//! * [`open`] / [`close`] / [`wait`] / [`recover`]: one browser under the lock.
//! * [`pack`]: the data directory as a `.tar.gz` and back, caches left out.

mod error;
mod hub;
pub mod pack;
mod session;
mod state;

pub use error::{Error, LockedBy, Result};
pub use hub::{Entry, Hub, LockRow, PullReport, PushReport, delete, edit, log_out, unbind};
pub use session::{
    CloseReport, Outcome, catch_signals, close, is_syncing, open, recover, shutdown, take_finished, wait,
};
pub use state::{Bound, Login, bound_ids, load_login};
