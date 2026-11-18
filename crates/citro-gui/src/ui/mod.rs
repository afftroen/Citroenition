//! The egui layer. `app` owns the state and the frame loop; every other module is one page or dialog and only
//! draws + returns what the user asked for. Blocking backend work goes through `tasks`.

pub mod app;
pub mod bulk;
pub mod dialogs;
pub mod edit;
pub mod export;
pub mod extensions;
pub mod groups;
pub mod import;
pub mod kernel;
pub mod logs;
pub mod messages;
pub mod plugins;
pub mod profiles;
pub mod proxy_dialog;
pub mod settings;
pub mod shell;
pub mod shot;
pub mod tags;
pub mod tasks;
pub mod teams;
pub mod theme;
pub mod update;
pub mod widgets;

pub use app::{AppOptions, CitroApp, Page};
#[cfg(test)]
mod headless_tests;
