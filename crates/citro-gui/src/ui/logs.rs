use super::app::CitroApp;
use crate::backend::LogLine;
use eframe::egui::Ui;

#[derive(Default)]
pub struct State {}

impl State {
    pub fn show_browser(&mut self) {}
    pub fn show_browser_for(&mut self, _id: &str) {}
    pub fn mark_dirty(&mut self) {}
    pub fn apply(&mut self, _token: u64, _r: Result<Vec<LogLine>, String>) {}
}

impl CitroApp {
    pub fn logs_page(&mut self, _ui: &mut Ui) {}
}
