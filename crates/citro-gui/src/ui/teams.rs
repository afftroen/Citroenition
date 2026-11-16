use super::app::CitroApp;
use eframe::egui::Ui;

#[derive(Default)]
pub struct State {
    pub connecting: bool,
    pub error: Option<String>,
}

impl CitroApp {
    pub fn teams_page(&mut self, _ui: &mut Ui) {}
}
