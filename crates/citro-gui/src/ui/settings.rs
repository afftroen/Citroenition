use super::app::CitroApp;
use eframe::egui::Ui;

#[derive(Default)]
pub struct State {}

impl CitroApp {
    pub fn settings_page(&mut self, _ui: &mut Ui) {}
}
