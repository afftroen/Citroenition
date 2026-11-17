//! The Edit proxy window, opened from the edit mark in a profile's IP cell: type, host and port, login, a check.

use super::app::CitroApp;
use super::tasks::Msg;
use super::widgets::{self, dim, semibold, small_dim};
use crate::backend::{BResult, ProxyCheck};
use crate::proxy::{self, ProxyKind};
use citro_core::profile::{Profile, ProxyConfig};
use eframe::egui::{self, Align, ComboBox, Layout, Margin, RichText, TextEdit, Ui, vec2};

const LABEL_W: f32 = 120.0;

pub struct ProxyDialog {
    pub id: String,
    serial: u64,
    name: String,
    kind: ProxyKind,
    host: String,
    port: String,
    user: String,
    pass: String,
    paste: String,
    paste_err: Option<String>,
    loading: bool,
    checking: bool,
    token: u64,
    check: Option<Result<ProxyCheck, String>>,
    error: Option<String>,
}

impl ProxyDialog {
    fn fill(&mut self, p: &Profile) {
        match p.user_proxy_config.hop() {
            None => {
                self.kind = ProxyKind::Direct;
                (self.host, self.port, self.user, self.pass) = Default::default();
            }
            Some(h) => {
                self.kind = ProxyKind::from_scheme(&h.scheme).unwrap_or(ProxyKind::Http);
                self.host = h.host;
                self.port = h.port.to_string();
                self.user = h.account;
                self.pass = if h.password == crate::backend::MASK { String::new() } else { h.password };
            }
        }
    }

    /// The proxy the form describes, or what is wrong with it.
    pub fn config(&self) -> Result<ProxyConfig, String> {
        if self.kind == ProxyKind::Direct {
            return Ok(ProxyConfig { proxy_soft: Some("no_proxy".into()), ..Default::default() });
        }
        let host = self.host.trim();
        if host.is_empty() {
            return Err("Enter the proxy's host".into());
        }
        let port: u16 =
            self.port.trim().parse().ok().filter(|p| *p > 0).ok_or("The port is a number from 1 to 65535")?;
        Ok(ProxyConfig {
            proxy_soft: Some("other".into()),
            proxy_type: Some(self.kind.scheme().into()),
            proxy_host: Some(host.to_owned()),
            proxy_port: Some(port.to_string()),
            proxy_user: Some(self.user.trim().to_owned()).filter(|u| !u.is_empty()),
            proxy_password: Some(self.pass.clone()).filter(|u| !u.is_empty()),
            ..Default::default()
        })
    }

    fn url(&self) -> String {
        let login = match (self.user.trim(), self.pass.as_str()) {
            ("", _) => String::new(),
            (u, "") => format!("{u}@"),
            (u, p) => format!("{u}:{p}@"),
        };
        format!("{}://{login}{}:{}", self.kind.scheme(), self.host.trim(), self.port.trim())
    }
}

fn row<R>(ui: &mut Ui, p: &crate::ui::theme::Palette, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(vec2(LABEL_W, 34.0), Layout::right_to_left(Align::Center), |ui| {
            ui.set_min_width(LABEL_W);
            ui.label(dim(p, label));
        });
        ui.add_space(14.0);
        add(ui)
    })
    .inner
}

fn field(text: &mut String) -> TextEdit<'_> {
    TextEdit::singleline(text).margin(Margin::symmetric(10, 8))
}

impl CitroApp {
    pub fn open_proxy_dialog(&mut self, id: &str) {
        let Some(r) = self.row(id) else { return };
        let mut d = ProxyDialog {
            id: id.to_owned(),
            serial: r.profile.serial_number,
            name: r.profile.name.clone(),
            kind: ProxyKind::Direct,
            host: String::new(),
            port: String::new(),
            user: String::new(),
            pass: String::new(),
            paste: String::new(),
            paste_err: None,
            loading: true,
            checking: false,
            token: 0,
            check: None,
            error: None,
        };
        d.fill(&r.profile);
        self.proxy_dlg = Some(d);
        // the list only has the masked password: the full profile has the real one
        let id = id.to_owned();
        self.tasks.spawn(false, move |w| {
            let result = w.backend.profile(&id);
            w.send(Msg::ProxyDialogLoaded { id, result });
        });
    }

    pub fn proxy_dialog_loaded(&mut self, id: &str, r: BResult<Profile>) {
        if let Some(d) = self.proxy_dlg.as_mut().filter(|d| d.id == id) {
            d.loading = false;
            match r {
                Ok(p) => d.fill(&p),
                Err(e) => d.error = Some(e.to_string()),
            }
        }
    }

    pub fn proxy_dialog_checked(&mut self, token: u64, r: BResult<ProxyCheck>) {
        if let Some(d) = self.proxy_dlg.as_mut().filter(|d| d.token == token) {
            d.checking = false;
            d.check = Some(r.map_err(|e| e.to_string()));
        }
    }

    pub fn proxy_modal(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.proxy_dlg.take() else { return };
        let p = self.pal;
        let (mut ok, mut cancel, mut check) = (false, false, false);
        let r = widgets::modal(ctx, &p, "proxy-modal", 640.0, |ui| {
            ui.label(semibold("Edit proxy", 19.0));
            ui.add_space(14.0);
            ui.spacing_mut().item_spacing.y = 12.0;
            row(ui, &p, "Profile No.", |ui| {
                widgets::badge(ui, &d.serial.to_string(), p.accent_hover);
                ui.label(dim(&p, &d.name));
                if d.loading {
                    ui.add(egui::Spinner::new().size(12.0));
                }
            });
            row(ui, &p, "Quick paste", |ui| {
                let r = ui.add(
                    field(&mut d.paste)
                        .hint_text("socks5://user:pass@host:port or host:port:user:pass")
                        .desired_width(400.0),
                );
                if r.changed() && !d.paste.trim().is_empty() {
                    match proxy::parse(d.paste.trim()) {
                        Ok(pp) => {
                            if let Some(k) = pp.kind {
                                d.kind = k;
                            } else if d.kind == ProxyKind::Direct {
                                d.kind = ProxyKind::Http;
                            }
                            (d.host, d.port, d.user, d.pass) = (pp.host, pp.port.to_string(), pp.user, pp.password);
                            d.paste_err = None;
                            d.check = None;
                        }
                        Err(e) => d.paste_err = Some(e),
                    }
                }
            });
            if let Some(e) = &d.paste_err {
                row(ui, &p, "", |ui| ui.label(RichText::new(e).color(p.err).size(12.0)));
            }
            row(ui, &p, "Proxy type", |ui| {
                ComboBox::from_id_salt("proxy-dlg-kind")
                    .width(330.0)
                    .icon(widgets::chevron_icon)
                    .selected_text(d.kind.label())
                    .show_ui(ui, |ui| {
                        for k in ProxyKind::ALL {
                            if ui.selectable_value(&mut d.kind, k, k.label()).changed() {
                                d.check = None;
                            }
                        }
                    });
            });
            if d.kind != ProxyKind::Direct {
                row(ui, &p, "Host:Port", |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let a = ui.add(field(&mut d.host).hint_text("host").desired_width(230.0));
                    ui.label(dim(&p, ":"));
                    let b = ui.add(field(&mut d.port).hint_text("port").desired_width(80.0));
                    if a.changed() || b.changed() {
                        d.check = None;
                    }
                    ui.add_space(6.0);
                    if ui.add_enabled(!d.checking, egui::Button::new("Check proxy")).clicked() {
                        check = true;
                    }
                    if ui
                        .small_button("Copy")
                        .on_hover_text("Copy as a proxy address: type://user:pass@host:port")
                        .clicked()
                    {
                        ui.ctx().copy_text(d.url());
                    }
                });
                row(ui, &p, "Proxy username", |ui| {
                    ui.add(field(&mut d.user).hint_text("optional").desired_width(330.0)).changed()
                });
                row(ui, &p, "Proxy password", |ui| {
                    ui.add(field(&mut d.pass).hint_text("optional").desired_width(330.0)).changed()
                });
            }
            match &d.check {
                _ if d.checking => {
                    row(ui, &p, "", |ui| {
                        ui.add(egui::Spinner::new().size(13.0));
                        ui.label(dim(&p, "Checking\u{2026}"));
                    });
                }
                Some(Ok(c)) if c.ok => {
                    let place = crate::countries::name(&c.country.to_ascii_lowercase())
                        .map(|n| format!(" \u{b7} {n}"))
                        .unwrap_or_default();
                    row(ui, &p, "", |ui| {
                        ui.label(
                            RichText::new(format!(
                                "\u{2713} Works: exit IP {}{place} \u{b7} {} ms",
                                c.ip, c.latency_ms
                            ))
                            .color(p.ok),
                        )
                    });
                }
                Some(Ok(c)) => {
                    row(ui, &p, "", |ui| ui.label(RichText::new(format!("\u{2717} {}", c.message)).color(p.err)));
                }
                Some(Err(e)) => {
                    row(ui, &p, "", |ui| ui.label(RichText::new(format!("\u{2717} {e}")).color(p.err)));
                }
                None => {}
            }
            if let Some(e) = &d.error {
                row(ui, &p, "", |ui| ui.label(RichText::new(e).color(p.err)));
            }
            if self.row(&d.id).is_some_and(|r| r.run.is_open()) {
                row(ui, &p, "", |ui| ui.label(small_dim(&p, "The new proxy is used the next time the profile opens.")));
            }
            ui.add_space(6.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.add_space(LABEL_W + 14.0);
                if widgets::primary_button(ui, &p, "OK", !d.loading).clicked() {
                    ok = true;
                }
                if widgets::button(ui, "Cancel", true).clicked() {
                    cancel = true;
                }
            });
        });
        if cancel || r.should_close() {
            return;
        }
        if check {
            match d.config() {
                Ok(cfg) => {
                    d.token += 1;
                    d.checking = true;
                    d.check = None;
                    let token = d.token;
                    self.tasks.spawn(true, move |w| {
                        w.send(Msg::ProxyDialogChecked { token, result: w.backend.check_proxy(&cfg) });
                    });
                }
                Err(e) => d.check = Some(Err(e)),
            }
        }
        if ok {
            match d.config() {
                Ok(cfg) => {
                    // saved, then checked: the IP column shows the result
                    self.check_row_proxy(d.id.clone(), Some(cfg));
                    return;
                }
                Err(e) => d.error = Some(e),
            }
        }
        self.proxy_dlg = Some(d);
    }
}
