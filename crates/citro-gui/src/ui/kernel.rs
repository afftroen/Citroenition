use super::app::CitroApp;
use eframe::egui::Ui;

#[derive(Default)]
pub struct State {
    pub progress: Option<(u64, u64)>,
    pub installing: bool,
    pub error: Option<String>,
}

impl CitroApp {
    pub fn kernel_page(&mut self, _ui: &mut Ui) {}
}
