use super::app::CitroApp;
use eframe::egui;

pub struct ExportDialog {}

impl ExportDialog {
    pub fn finished(&mut self, _path: &str, _rows: usize, _r: Result<(), String>) {}
}

impl CitroApp {
    pub fn open_export(&mut self) {}
    pub fn export_modal(&mut self, _ctx: &egui::Context) {}
}
