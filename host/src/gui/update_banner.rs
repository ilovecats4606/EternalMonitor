//! The update banner above every page, the update popup, the update rows in
//! Settings, and the notice shown when an iPad cannot connect because of a
//! version difference.

use eframe::egui::{self, vec2, Color32, Frame, Margin, Rect, Sense, Stroke, Ui};

use super::icons::{self, Icon};
use super::theme::*;
use super::widgets::{self, Tone, GAP};
use super::{update_toast, AnalyzerApp, ClientSnapshot, Page, StatsSnapshot, StreamState};
use crate::updates::{CheckStatus, Notice, OlderSide, WEBSITE_DOWNLOAD};

/// The popup's last line: what Update now does from here.
pub(super) fn popup_hint(installs_in_place: bool, streaming: bool) -> &'static str {
    if !installs_in_place {
        "Update now opens the download page."
    } else if streaming {
        "Your iPad stays connected. The update installs when you stop streaming."
    } else {
        "Windows will ask for permission, then EternalMonitor restarts."
    }
}

impl AnalyzerApp {
    pub(super) fn update_banner(
        &mut self,
        ui: &mut Ui,
        snap: &StatsSnapshot,
        client: &ClientSnapshot,
    ) {
        let streaming = StreamState::of(snap, client) == StreamState::Streaming;
        if self.updates.tick(streaming) {
            // Setup is waiting for this process to exit. Close the normal way
            // so the stream, virtual display and mDNS shut down cleanly.
            update_toast::hide();
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if streaming {
            // A connected iPad means the versions work together.
            crate::updates::clear_mismatch();
        } else if let Some(side) = crate::updates::recent_mismatch() {
            self.mismatch_banner(ui, side);
        }
        let Some(notice) = self.updates.notice() else {
            return;
        };
        let mut action = None;
        match &notice {
            Notice::Available(version) => widgets::banner(
                ui,
                ACCENT,
                Icon::Download,
                &format!("EternalMonitor {version} is available"),
                if self.updates.installs_in_place() {
                    "Windows will ask for permission, then EternalMonitor restarts. Update the iPad app in TestFlight too."
                } else {
                    "Update now opens the download page. Install the new version, then open EternalMonitor again. Update the iPad app in TestFlight too."
                },
                190.0,
                |ui| {
                    if widgets::button(ui, Tone::Ghost, None, "Later").clicked() {
                        action = Some(Action::Later);
                    }
                    if widgets::button(ui, Tone::Primary, None, "Update now").clicked() {
                        action = Some(Action::UpdateNow);
                    }
                },
            ),
            Notice::Queued(_) => widgets::banner(
                ui,
                ACCENT,
                Icon::Download,
                "The update will install when you stop streaming",
                "Your iPad stays connected until then. Stop and update closes the stream now.",
                230.0,
                |ui| {
                    if widgets::button(ui, Tone::Ghost, None, "Cancel").clicked() {
                        action = Some(Action::Cancel);
                    }
                    if widgets::button(ui, Tone::Secondary, None, "Stop and update").clicked() {
                        action = Some(Action::Force);
                    }
                },
            ),
            Notice::Downloading(version, fraction) => {
                let body = match fraction {
                    Some(fraction) => format!("{:.0}% downloaded.", fraction * 100.0),
                    None => "Starting the download.".into(),
                };
                widgets::banner(
                    ui,
                    ACCENT,
                    Icon::Download,
                    &format!("Downloading EternalMonitor {version}"),
                    &body,
                    170.0,
                    |ui| {
                        ui.add(
                            egui::ProgressBar::new(fraction.unwrap_or(0.0))
                                .desired_width(160.0)
                                .fill(ACCENT),
                        );
                    },
                );
            }
            Notice::Installing(version) => widgets::banner(
                ui,
                ACCENT,
                Icon::Download,
                &format!("Installing EternalMonitor {version}"),
                "If Windows asks for permission, click Yes. EternalMonitor closes and opens again by itself.",
                0.0,
                |_| {},
            ),
            Notice::Failed(message) => widgets::banner(
                ui,
                WARNING,
                Icon::Warning,
                "The update did not finish",
                message,
                340.0,
                |ui| {
                    if widgets::button(ui, Tone::Ghost, None, "Later").clicked() {
                        action = Some(Action::Later);
                    }
                    if widgets::button(ui, Tone::Secondary, None, "Try again").clicked() {
                        action = Some(Action::UpdateNow);
                    }
                    widgets::link(ui, "Download from the website", WEBSITE_DOWNLOAD);
                },
            ),
            Notice::Updated(version) => widgets::banner(
                ui,
                ACCENT,
                Icon::Check,
                &format!("EternalMonitor is updated to {version}"),
                "If the iPad app asks for an update, open TestFlight on the iPad and tap Update.",
                60.0,
                |ui| {
                    if widgets::button(ui, Tone::Secondary, None, "OK").clicked() {
                        action = Some(Action::Later);
                    }
                },
            ),
        }
        ui.add_space(GAP - 10.0);
        if let Some(action) = action {
            self.update_action(ui.ctx(), action);
        }
    }

    /// A small card over the window, once per version and then at most every
    /// three days (see `updates::State::popup`). It opens only on the Stream
    /// page and not mid-click, mid-drag or while typing; streaming is fine,
    /// since Update now waits for the stream like the banner's button.
    pub(super) fn update_popup(
        &mut self,
        ctx: &egui::Context,
        snap: &StatsSnapshot,
        client: &ClientSnapshot,
    ) {
        let Some(popup) = self.updates.popup() else {
            self.update_popup_open = false;
            return;
        };
        if !self.update_popup_open {
            let busy = self.page != Page::Stream
                || ctx.egui_wants_keyboard_input()
                || ctx.input(|i| i.pointer.any_down());
            if busy {
                return;
            }
            self.update_popup_open = true;
        }
        let streaming = StreamState::of(snap, client) == StreamState::Streaming;
        let hint = popup_hint(self.updates.installs_in_place(), streaming);
        let mut action = None;
        let modal = egui::Modal::new(egui::Id::new("update-popup"))
            .backdrop_color(Color32::from_black_alpha(140))
            .frame(
                Frame::new()
                    .fill(SURFACE_RAISED)
                    .stroke(Stroke::new(1.0, BORDER_STRONG))
                    .corner_radius(16.0)
                    .inner_margin(Margin::same(24))
                    .shadow(egui::Shadow {
                        offset: [0, 20],
                        blur: 56,
                        spread: 0,
                        color: Color32::from_black_alpha(160),
                    }),
            )
            .show(ctx, |ui| {
                ui.ctx().accesskit_node_builder(ui.unique_id(), |node| {
                    node.set_role(egui::accesskit::Role::Dialog);
                    node.set_label("Update ready");
                });
                ui.set_width(340.0);
                ui.spacing_mut().item_spacing.y = 6.0;
                let (badge, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                ui.painter()
                    .circle_filled(badge.center(), 20.0, mix(SURFACE_RAISED, ACCENT, 14));
                icons::paint(
                    ui.painter(),
                    Rect::from_center_size(badge.center(), vec2(20.0, 20.0)),
                    Icon::Download,
                    ACCENT,
                );
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(popup.title())
                            .family(semibold())
                            .size(18.0)
                            .color(TEXT),
                    )
                    .wrap(),
                );
                ui.add(egui::Label::new(strong(popup.message())).wrap());
                if let Some(note) = &popup.note {
                    ui.add(egui::Label::new(muted(note)).wrap());
                }
                ui.add(egui::Label::new(faint(hint)).wrap());
                ui.add_space(14.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::button(ui, Tone::Primary, None, "Update now").clicked() {
                        action = Some(Action::UpdateNow);
                    }
                    if widgets::button(ui, Tone::Ghost, None, "Later").clicked() {
                        action = Some(Action::Later);
                    }
                });
            });
        // Escape or a click beside the card means Later.
        if action.is_none() && modal.should_close() {
            action = Some(Action::Later);
        }
        if let Some(action) = action {
            self.updates.popup_closed();
            self.update_popup_open = false;
            if matches!(action, Action::UpdateNow) {
                self.update_action(ctx, action);
            }
        }
    }

    fn update_action(&mut self, ctx: &egui::Context, action: Action) {
        update_toast::hide();
        match action {
            Action::UpdateNow if !self.updates.installs_in_place() => {
                ctx.open_url(egui::OpenUrl::new_tab(WEBSITE_DOWNLOAD));
            }
            Action::UpdateNow => self.updates.update_now(),
            Action::Force => self.updates.force_now(),
            Action::Cancel => self.updates.cancel(),
            Action::Later => self.updates.later(),
        }
    }

    fn mismatch_banner(&mut self, ui: &mut Ui, side: OlderSide) {
        let (title, body) = crate::updates::mismatch_text(side);
        let mut action = None;
        match side {
            OlderSide::Pc => {
                let available = self.updates.available_version().is_some();
                widgets::banner(ui, WARNING, Icon::Warning, title, body, 300.0, |ui| {
                    widgets::link(ui, "Download from the website", WEBSITE_DOWNLOAD);
                    let label = if available {
                        "Update now"
                    } else {
                        "Check for updates"
                    };
                    if widgets::button(ui, Tone::Primary, None, label).clicked() {
                        action = Some(available);
                    }
                });
            }
            OlderSide::Ipad => {
                widgets::banner(ui, WARNING, Icon::Warning, title, body, 60.0, |ui| {
                    if widgets::button(ui, Tone::Secondary, None, "OK").clicked() {
                        crate::updates::clear_mismatch();
                    }
                });
            }
        }
        match action {
            Some(true) => self.update_action(ui.ctx(), Action::UpdateNow),
            Some(false) => self.updates.check_now(),
            None => {}
        }
        ui.add_space(GAP - 10.0);
    }

    /// The rows in Settings → General.
    pub(super) fn update_settings(&mut self, ui: &mut Ui) {
        let mut automatic = self.settings_check_updates;
        widgets::setting_row(
            ui,
            "Check for updates automatically",
            "Looks for a new version once a day and tells you when one is ready.",
            64.0,
            |ui| {
                widgets::toggle(ui, &mut automatic, "Automatic update checks");
            },
        );
        if automatic != self.settings_check_updates {
            self.set_check_updates(automatic);
        }
        widgets::divider(ui);
        let status = self.updates.check_status();
        let available = self.updates.available_version();
        let description = match (status, &available) {
            (CheckStatus::Checking, _) => "Checking for updates…".to_string(),
            (_, Some(version)) => format!("Version {version} is ready to install."),
            (CheckStatus::UpToDate, None) => "You have the latest version.".to_string(),
            (CheckStatus::Failed, None) => {
                "Could not check for updates. Check your internet connection.".to_string()
            }
            _ => "Click Check now to look for a new version.".to_string(),
        };
        let mut clicked = false;
        widgets::setting_row(
            ui,
            &format!("Version {}", env!("CARGO_PKG_VERSION")),
            &description,
            110.0,
            |ui| {
                let label = if available.is_some() {
                    "Update now"
                } else {
                    "Check now"
                };
                clicked = widgets::button_ex(
                    ui,
                    Tone::Secondary,
                    None,
                    label,
                    status != CheckStatus::Checking,
                    0.0,
                )
                .clicked();
            },
        );
        if clicked {
            if available.is_some() {
                self.update_action(ui.ctx(), Action::UpdateNow);
            } else {
                self.updates.check_now();
            }
        }
    }
}

enum Action {
    UpdateNow,
    Force,
    Cancel,
    Later,
}
