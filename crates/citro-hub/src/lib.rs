//! Citroenition team hub: a small self-hosted server that lets several people share browser profiles.
//!
//! * [`model`] — the wire/state types, shared by the server and the client.
//! * [`server`] — the hub itself (state file, locks and leases, hosted profile data, logs, CSV export, HTTP API).
//! * [`client`] — a blocking, typed client for the GUI / CLI (`ureq`), including a lease keep-alive helper.
//!
//! See `docs/HUB.md` for the concepts, the HTTP contract and the operating notes.

pub mod cli;
pub mod client;
pub mod model;
pub mod server;
