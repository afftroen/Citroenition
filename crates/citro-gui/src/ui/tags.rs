//! The Tags window: tick tags for the chosen profiles, create tags (a name of at most 50 characters and a color),
//! recolor and delete them.

use super::app::CitroApp;
use super::tasks::Refresh;
use super::widgets::{self, dim, small_dim};
use crate::backend::Tag;
use citro_core::profile::{TAG_COLORS, TAG_MAX, tag_rgb};
use eframe::egui::{self, Color32, CornerRadius, CursorIcon, Margin, RichText, Sense, TextEdit, Ui, vec2};

fn rgb(key: &str) -> Color32 {
    let c = tag_rgb(key);
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// A row of color dots; returns the clicked key.
fn swatches(ui: &mut Ui, current: &str) -> Option<String> {
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (key, _) in TAG_COLORS {
            let (r, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
            ui.painter().circle_filled(r.center(), 7.5, rgb(key));
            if key == current {
                ui.painter().circle_stroke(r.center(), 9.0, egui::Stroke::new(1.5, ui.visuals().strong_text_color()));
            }
            if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(key).clicked() {
                picked = Some(key.to_owned());
            }
        }
    });
    picked
}

impl CitroApp {
    pub fn tags_modal(&mut self, ctx: &egui::Context) {
        let Some(ids) = self.profiles.tags_for.clone() else { return };
        let p = self.pal;
        let tags = self.tags.clone();
        let have =
            |t: &Tag| ids.iter().filter(|id| self.row(id).is_some_and(|r| r.profile.tags.contains(&t.id))).count();
        let counts: Vec<usize> = tags.iter().map(have).collect();
        let n = ids.len();
        let (mut toggle, mut recolor, mut delete, mut create, mut close) = (None, None, None, false, false);
        let r = widgets::modal(ctx, &p, "tags-modal", 460.0, |ui| {
            let title =
                if n == 1 { format!("Tags of {}", self.name_of(&ids[0])) } else { format!("Tags of {n} profiles") };
            ui.label(widgets::semibold(title, 17.0));
            ui.add_space(10.0);
            if tags.is_empty() {
                ui.label(dim(&p, "No tags yet: create the first one below."));
            }
            egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                for (t, &c) in tags.iter().zip(&counts) {
                    ui.horizontal(|ui| {
                        // all / some / none of the profiles have it
                        let all = c == n;
                        let mut on = all;
                        let mixed = c > 0 && !all;
                        if widgets::checkbox(ui, &p, on, mixed).clicked() {
                            on = !all;
                            toggle = Some((t.id.clone(), on));
                        }
                        widgets::badge(ui, &t.name, rgb(&t.color));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button(RichText::new("Delete").color(p.err)).clicked() {
                                delete = Some(t.id.clone());
                            }
                            let dot = ui
                                .add(egui::Button::new(RichText::new("\u{25cf}").color(rgb(&t.color))).small())
                                .on_hover_text("Change the color");
                            egui::Popup::menu(&dot).show(|ui| {
                                if let Some(k) = swatches(ui, &t.color) {
                                    recolor = Some(Tag { color: k, ..t.clone() });
                                    ui.close();
                                }
                            });
                        });
                    });
                }
            });
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label(widgets::semibold("New tag", 13.5));
            ui.add_space(4.0);
            let s = &mut self.profiles;
            if s.new_tag_color.is_empty() {
                s.new_tag_color = TAG_COLORS[tags.len() % TAG_COLORS.len()].0.to_owned();
            }
            ui.horizontal(|ui| {
                let r = ui.add(
                    TextEdit::singleline(&mut s.new_tag)
                        .char_limit(TAG_MAX)
                        .hint_text("e.g. Ready, Banned, TikTok")
                        .margin(Margin::symmetric(10, 7))
                        .desired_width(250.0),
                );
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let ok = !s.new_tag.trim().is_empty();
                if widgets::primary_button(ui, &p, "Create", ok).clicked() || (enter && ok) {
                    create = true;
                }
            });
            ui.label(small_dim(&p, format!("{} / {TAG_MAX}", s.new_tag.chars().count())));
            if let Some(k) = swatches(ui, &s.new_tag_color.clone()) {
                s.new_tag_color = k;
            }
            ui.horizontal(|ui| {
                ui.label(small_dim(&p, "Preview"));
                let name = if s.new_tag.trim().is_empty() { "New tag" } else { s.new_tag.trim() };
                widgets::badge(ui, name, rgb(&s.new_tag_color));
            });
            widgets::footer(ui, |ui| {
                if widgets::button(ui, "Done", true).clicked() {
                    close = true;
                }
            });
        });
        let _ = CornerRadius::ZERO;
        if close || r.should_close() {
            self.profiles.tags_for = None;
            self.profiles.new_tag.clear();
        }
        if let Some((tid, on)) = toggle {
            let (add, remove) = if on { (vec![tid], vec![]) } else { (vec![], vec![tid]) };
            for row in self.rows.iter_mut().filter(|r| ids.iter().any(|i| i == r.id())) {
                row.profile.tags.retain(|t| !remove.contains(t));
                for a in &add {
                    if !row.profile.tags.contains(a) {
                        row.profile.tags.push(a.clone());
                    }
                }
            }
            let ids = ids.clone();
            self.run_op(Vec::new(), String::new(), Refresh::ROWS, move |b| b.retag(&ids, &add, &remove));
        }
        if let Some(t) = recolor {
            if let Some(x) = self.tags.iter_mut().find(|x| x.id == t.id) {
                x.color = t.color.clone();
            }
            self.run_op(Vec::new(), String::new(), Refresh::ROWS_GROUPS, move |b| b.save_tag(t).map(|_| ()));
        }
        if let Some(tid) = delete {
            self.tags.retain(|x| x.id != tid);
            self.run_op(Vec::new(), "Tag deleted".into(), Refresh::ROWS_GROUPS, move |b| b.delete_tag(&tid));
        }
        if create {
            let name: String = self.profiles.new_tag.trim().chars().take(TAG_MAX).collect();
            let color = std::mem::take(&mut self.profiles.new_tag_color);
            self.profiles.new_tag.clear();
            let ids = ids.clone();
            self.run_op(Vec::new(), format!("Created tag {name}"), Refresh::ROWS_GROUPS, move |b| {
                let t = b.save_tag(Tag { id: String::new(), name, color })?;
                b.retag(&ids, &[t.id], &[])
            });
        }
    }
}
