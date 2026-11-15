// Temporary GL/Xvfb probe: replaced by the real entry point.
use eframe::egui;
use std::sync::{Arc, mpsc};

struct Probe {
    frame: u32,
    rx: mpsc::Receiver<Arc<egui::ColorImage>>,
    tx: mpsc::Sender<Arc<egui::ColorImage>>,
    out: String,
}

impl eframe::App for Probe {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::CentralPanel::default_margins().show(ui, |ui| {
            ui.heading("Citroenition probe é ü ß ç ñ");
            let set = "· … – — × → ← ↑ ↓ • ✓ ▾ ▴ ▸ ⋯ « » € ° ± ≥ ≤ № ✕ ● ○ ■ □ ▲ ▼ ★ ☰ ⌘ ↵ ⏎ ⌫";
            ui.label(set);
            ui.label(egui::RichText::new(set).monospace());
            ui.label(egui::RichText::new("ABC abc 0123456789 [ ] { } ( ) < > / \\ | _ ~ ^ @ # $ % & * + = ?").size(15.0));
            ui.label("Hello from egui under Xvfb");
            let _ = ui.button("Open");
        });
        self.frame += 1;
        ctx.request_repaint();
        if self.frame == 10 {
            let tx = self.tx.clone();
            let c2 = ctx.clone();
            ctx.request_screenshot(move |img| {
                let _ = tx.send(img);
                c2.request_repaint();
            });
        }
        if let Ok(img) = self.rx.try_recv() {
            let (w, h) = (img.size[0] as u32, img.size[1] as u32);
            let mut rgba = Vec::with_capacity((w * h * 4) as usize);
            for p in &img.pixels {
                rgba.extend_from_slice(&[p.r(), p.g(), p.b(), p.a()]);
            }
            let file = std::fs::File::create(&self.out).unwrap();
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(&rgba).unwrap();
            println!("screenshot {}x{} -> {}", w, h, self.out);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn main() -> eframe::Result {
    let out = std::env::args().nth(1).unwrap_or_else(|| "/tmp/probe.png".into());
    let (tx, rx) = mpsc::channel();
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native("probe", opts, Box::new(move |_cc| Ok(Box::new(Probe { frame: 0, rx, tx, out }))))
}
