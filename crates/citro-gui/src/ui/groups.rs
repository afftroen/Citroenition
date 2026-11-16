use super::app::CitroApp;
use citro_core::profile::Group;
use eframe::egui::Ui;

#[derive(Default)]
pub struct State {}

impl State {
    pub fn created(&mut self, _g: &Group) {}
}

impl CitroApp {
    pub fn groups_page(&mut self, _ui: &mut Ui) {}
}
