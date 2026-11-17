//! The real app, driven frame by frame without a window.

use super::app::{AppOptions, CitroApp};
use crate::backend::Backend;
use citro_core::profile::Profile;
use citro_core::store::{self, Home};
use eframe::egui;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn frames(app: &mut CitroApp, ctx: &egui::Context, n: usize) {
    let mut frame = eframe::Frame::_new_kittest();
    for _ in 0..n {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0))),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| eframe::App::ui(app, ui, &mut frame));
        out.textures_delta.clear();
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn deleting_profiles_keeps_the_app_running() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().to_path_buf() };
    for i in 0..6 {
        store::locked(&home, |s| {
            s.create(Profile { name: format!("p{i}"), remark: format!("r{i}"), ..Default::default() })
        })
        .unwrap();
    }
    let backend: Arc<dyn Backend> = Arc::new(crate::backend::local::LocalBackend::new(home.clone()));
    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut app = CitroApp::new(
        &cc,
        AppOptions {
            backend,
            settings: Default::default(),
            settings_path: None,
            start: Default::default(),
            screenshot: None,
            demo_failing: Vec::new(),
        },
    );
    frames(&mut app, &ctx, 30);
    assert_eq!(app.rows.len(), 6);
    // select two and delete them, as the trash button + Delete does
    let ids: Vec<String> = app.rows.iter().take(2).map(|r| r.id().to_owned()).collect();
    app.confirm_delete_profiles(ids);
    app.test_run_confirm();
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(5) && app.rows.len() != 4 {
        frames(&mut app, &ctx, 5);
    }
    frames(&mut app, &ctx, 20);
    assert_eq!(app.rows.len(), 4);
}
