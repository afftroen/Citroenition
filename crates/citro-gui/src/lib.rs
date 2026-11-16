//! Citroenition desktop app: an egui profile manager in pure Rust.
//!
//! * [`backend`] - the `Backend` trait the UI talks to, plus the local (citro-core) and demo implementations.
//! * [`csv`], [`proxy`], [`query`], [`selection`], [`timefmt`], [`util`] - pure logic with unit tests.
//! * [`ui`] - the egui pages.

pub mod backend;
pub mod cli;
pub mod countries;
pub mod csv;
pub mod eventlog;
pub mod installer;
pub mod proxy;
pub mod query;
pub mod selection;
pub mod settings;
pub mod timefmt;
pub mod ui;
pub mod util;
