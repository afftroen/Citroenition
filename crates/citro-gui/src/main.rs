//! `citro-gui`: the Citroenition desktop app. Pure Rust (egui), see docs/GUI.md.

use citro_core::store::Home;
use citro_gui::backend::Backend;
use citro_gui::backend::local::LocalBackend;
use citro_gui::backend::mock::MockBackend;
use citro_gui::cli::{Args, USAGE};
use citro_gui::settings::Settings;
use citro_gui::ui::{AppOptions, CitroApp};
use eframe::egui;
use std::sync::Arc;
use std::time::Duration;

fn main() -> eframe::Result {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("citro-gui: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if args.help {
        print!("{USAGE}");
        return Ok(());
    }

    let shooting = args.screenshot.is_some();
    let (backend, mut settings, settings_path, demo_failing): (
        Arc<dyn Backend>,
        Settings,
        Option<std::path::PathBuf>,
        Vec<String>,
    ) = if args.demo {
        // a screenshot must not wait for the pretend latency of the demo backend
        let latency = if shooting { Duration::from_millis(40) } else { Duration::from_millis(900) };
        let mock = MockBackend::demo().with_latency(latency);
        let failing = mock.failing_ids();
        (Arc::new(mock), Settings::default(), None, failing)
    } else {
        let home = Home::discover(args.home.as_deref());
        let path = home.root.join("gui-settings.json");
        (Arc::new(LocalBackend::new(home)), Settings::load(&path), Some(path), Vec::new())
    };
    if let Some(t) = args.theme {
        settings.theme = t;
    }
    if let Some(k) = &args.kernel {
        settings.kernel_path = k.clone();
    }

    let (w, h) = args.size.unwrap_or((1280.0, 800.0));
    let viewport = egui::ViewportBuilder::default()
        .with_title("Citroenition")
        .with_app_id("citroenition")
        .with_inner_size([w, h])
        .with_min_inner_size([960.0_f32.min(w), 600.0_f32.min(h)]);
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    let opts =
        AppOptions { backend, settings, settings_path, start: args.page, screenshot: args.screenshot, demo_failing };
    eframe::run_native("Citroenition", options, Box::new(move |cc| Ok(Box::new(CitroApp::new(cc, opts)))))
}
