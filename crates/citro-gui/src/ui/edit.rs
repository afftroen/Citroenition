use super::app::CitroApp;
use crate::backend::ProxyCheck;
use citro_core::profile::{Group, Profile};
use eframe::egui;

pub struct EditDialog {
    pub id: String,
    pub saving: bool,
    pub loading: bool,
    pub error: Option<String>,
}

impl EditDialog {
    pub fn fill(&mut self, _p: &Profile) {}
    pub fn group_created(&mut self, _g: &Group) {}
    pub fn proxy_checked(&mut self, _token: u64, _r: Result<ProxyCheck, String>) {}
}

impl CitroApp {
    pub fn open_edit(&mut self, _id: Option<String>) {}
    pub fn edit_modal(&mut self, _ctx: &egui::Context) {}
}
