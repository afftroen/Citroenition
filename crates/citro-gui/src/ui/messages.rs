//! Everything the worker threads report back: one `Msg` per finished operation, applied to the app state here.

use super::app::{CitroApp, ToastKind};
use super::dialogs::OpenFailure;
use super::tasks::Msg;
use crate::backend::OpenError;
use crate::timefmt;
use std::time::Instant;

impl CitroApp {
    pub(super) fn handle(&mut self, m: Msg) {
        match m {
            Msg::Rows(result) => {
                self.rows_inflight = false;
                self.last_poll = Instant::now();
                self.rows_loaded = true;
                match result {
                    Ok(rows) => {
                        self.rows_error = None;
                        if rows != self.rows {
                            self.last_error.retain(|id, _| rows.iter().any(|r| r.id() == id && !r.run.is_open()));
                            self.rows = rows;
                            self.rows_version += 1;
                        }
                    }
                    Err(e) => self.rows_error = Some(e.to_string()),
                }
            }
            Msg::Groups(result) => {
                if let Ok(g) = result
                    && g != self.groups
                {
                    self.groups = g;
                    self.groups_version += 1;
                }
            }
            Msg::Opened { id, name, result } => {
                self.busy.remove(&id);
                match result {
                    Ok(info) => {
                        self.toast(ToastKind::Ok, format!("Opened {name} (pid {})", info.pid));
                    }
                    Err(OpenError::LockedElsewhere { user, device, since }) => {
                        let when = timefmt::since(self.now, since, self.offset);
                        self.toast(ToastKind::Warn, format!("{name} is open on {device} by {user} since {when}"));
                    }
                    Err(OpenError::Other(message)) => {
                        let message = crate::util::mask_secrets_in_text(&message);
                        self.last_error.insert(id.clone(), crate::util::first_line(&message).to_owned());
                        if self.quiet_failures {
                            self.toast(ToastKind::Err, format!("Could not open {name}"));
                        } else {
                            self.failures.push(OpenFailure { id, name, message });
                        }
                    }
                }
                self.refresh_rows();
            }
            Msg::Closed { id, name, result } => {
                self.busy.remove(&id);
                match result {
                    Ok(()) => {
                        self.last_error.remove(&id);
                        self.toast(ToastKind::Ok, format!("Closed {name}"));
                    }
                    Err(e) => self.toast(ToastKind::Err, format!("Could not close {name}: {e}")),
                }
                self.refresh_rows();
            }
            Msg::Op(done) => {
                for id in &done.ids {
                    self.busy.remove(id);
                }
                match done.result {
                    Ok(()) => self.toast(ToastKind::Ok, done.ok),
                    Err(e) => self.toast(ToastKind::Err, e),
                }
                self.apply_refresh(done.refresh);
            }
            Msg::Saved { result, created } => match result {
                Ok(p) => {
                    self.edit = None;
                    let verb = if created { "Created" } else { "Saved" };
                    self.toast(ToastKind::Ok, format!("{verb} profile {}", p.name));
                    self.refresh_rows();
                    self.refresh_groups();
                }
                Err(e) => {
                    if let Some(d) = &mut self.edit {
                        d.saving = false;
                        d.error = Some(e.to_string());
                    }
                }
            },
            Msg::GroupCreated(result) => match result {
                Ok(g) => {
                    self.groups.push(g.clone());
                    if let Some(d) = &mut self.edit {
                        d.group_created(&g);
                    }
                    self.groups_ui.created(&g);
                    self.toast(ToastKind::Ok, format!("Created group {}", g.group_name));
                    self.refresh_groups();
                }
                Err(e) => {
                    if let Some(d) = &mut self.edit {
                        d.error = Some(e.to_string());
                    }
                    self.toast(ToastKind::Err, e.to_string());
                }
            },
            Msg::FullProfile { id, result } => {
                if let Some(d) = self.edit.as_mut().filter(|d| d.id == id) {
                    match result {
                        Ok(p) => d.fill(&p),
                        Err(e) => {
                            d.loading = false;
                            d.error = Some(e.to_string());
                        }
                    }
                }
            }
            Msg::ProxyChecked { token, result } => {
                if let Some(d) = &mut self.edit {
                    d.proxy_checked(token, result.map_err(|e| e.to_string()));
                }
            }
            Msg::Exported { path, rows, result } => {
                if let Some(d) = &mut self.export {
                    d.finished(&path, rows, result.clone());
                }
                match result {
                    Ok(()) => self.toast(ToastKind::Ok, format!("Exported {rows} profile(s) to {path}")),
                    Err(e) => self.toast(ToastKind::Err, format!("Export failed: {e}")),
                }
            }
            Msg::Logs { token, result } => self.logs_ui.apply(token, result.map_err(|e| e.to_string())),
            Msg::Kernel(result) => match result {
                Ok(st) => {
                    self.kernel = Some(st);
                    self.kernel_error = None;
                }
                Err(e) => self.kernel_error = Some(e.to_string()),
            },
            Msg::KernelProgress { done, total } => self.kernel_ui.progress = Some((done, total)),
            Msg::UpdateChecked(r) => self.update_checked(r),
            Msg::UpdateProgress { done, total } => self.update_progress(done, total),
            Msg::UpdateDone(r) => self.update_done(r),
            Msg::KernelInstalled(result) => {
                self.kernel_ui.installing = false;
                self.kernel_ui.progress = None;
                match result {
                    Ok(st) => {
                        let v = st.installed.as_ref().map(|k| k.version.clone()).unwrap_or_default();
                        self.kernel = Some(st);
                        self.kernel_ui.error = None;
                        self.toast(ToastKind::Ok, format!("Kernel {v} installed"));
                    }
                    Err(e) => {
                        self.kernel_ui.error = Some(e.to_string());
                        self.toast(ToastKind::Err, "Kernel install failed");
                    }
                }
            }
            Msg::Teams(result) => match result {
                Ok(t) => {
                    self.teams = Some(t);
                    self.teams_error = None;
                }
                Err(e) => self.teams_error = Some(e.to_string()),
            },
            Msg::Hub(h) => self.hub = h,
            Msg::HubDisconnected(result) => {
                self.teams_ui.working = false;
                match result {
                    Ok(h) => {
                        self.hub = h;
                        self.teams = None;
                        self.teams_error = None;
                        self.toast(ToastKind::Info, "Disconnected from the team hub");
                        self.refresh_rows();
                    }
                    Err(e) => self.toast(ToastKind::Err, e.to_string()),
                }
            }
            Msg::Pulled(result) => {
                self.teams_ui.working = false;
                match result {
                    Ok(sentence) => self.toast(ToastKind::Ok, format!("Team: {sentence}")),
                    Err(e) => self.toast(ToastKind::Err, e.to_string()),
                }
                self.refresh_teams();
                self.refresh_rows();
                self.refresh_groups();
            }
            Msg::Notices(list) => {
                for n in list {
                    let kind = match n.level {
                        crate::backend::Level::Error => ToastKind::Err,
                        crate::backend::Level::Warn => ToastKind::Warn,
                        _ => ToastKind::Info,
                    };
                    self.toast(kind, n.text);
                }
            }
            Msg::QuitDone(lines) => {
                self.quit = super::app::Quit::Done;
                for l in &lines {
                    self.backend.record(crate::backend::Level::Info, None, &format!("quit: {l}"));
                }
                self.ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
            }
            Msg::HubConnected(result) => {
                self.teams_ui.connecting = false;
                match result {
                    Ok(h) => {
                        self.hub = h;
                        self.teams_ui.error = None;
                        self.teams_ui.connected();
                        // remember the address and device name (never the token); other unsaved edits stay unsaved
                        let (url, device) = (self.hub.url.clone(), self.hub.device_name.clone());
                        for s in [&mut self.settings, &mut self.saved_settings] {
                            s.hub_url = url.clone();
                            if !device.is_empty() {
                                s.device_name = device.clone();
                            }
                        }
                        if let Some(path) = &self.settings_path {
                            let _ = self.saved_settings.save(path);
                        }
                        self.toast(ToastKind::Ok, "Connected to the team hub");
                        self.refresh_teams();
                        self.refresh_rows();
                    }
                    Err(e) => self.teams_ui.error = Some(e.to_string()),
                }
            }
        }
    }
}
