//! The `--screenshot` harness: let the app settle, grab one frame through egui, write a PNG and quit.

use super::app::CitroApp;
use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

pub struct Shot {
    pub out: PathBuf,
    pub frame: u32,
    pub started: Instant,
    pub requested: bool,
    pub tx: Sender<Arc<egui::ColorImage>>,
    pub rx: Receiver<Arc<egui::ColorImage>>,
}

impl CitroApp {
    pub(super) fn tick_shot(&mut self, ctx: &egui::Context) {
        let idle = self.tasks.idle() && self.scenario.is_none();
        let Some(shot) = &mut self.shot else { return };
        shot.frame += 1;
        ctx.request_repaint();
        let waited = shot.started.elapsed();
        if !shot.requested
            && ((shot.frame >= 10 && idle && waited >= Duration::from_millis(900)) || waited > Duration::from_secs(10))
        {
            shot.requested = true;
            self.toasts.clear();
            let (tx, c2) = (shot.tx.clone(), ctx.clone());
            ctx.request_screenshot(move |img| {
                let _ = tx.send(img);
                c2.request_repaint();
            });
        }
        if let Ok(img) = shot.rx.try_recv() {
            match write_png(&shot.out, &img) {
                Ok(()) => println!("screenshot {}x{} -> {}", img.size[0], img.size[1], shot.out.display()),
                Err(e) => {
                    eprintln!("citro-gui: cannot write {}: {e}", shot.out.display());
                    std::process::exit(2);
                }
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn write_png(path: &std::path::Path, img: &egui::ColorImage) -> Result<(), String> {
    let (w, h) = (img.size[0] as u32, img.size[1] as u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for px in &img.pixels {
        rgba.extend_from_slice(&[px.r(), px.g(), px.b(), 255]);
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().map_err(|e| e.to_string())?.write_image_data(&rgba).map_err(|e| e.to_string())
}
