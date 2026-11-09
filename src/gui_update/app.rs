//! The update GUI application shell.

use super::model::Backend;
use super::panels;
use super::state::{AppState, Tab};
use super::style;
use super::ui::{self, Tone};
use crate::gui::theme;
use eframe::egui;
use egui::{Align, Layout, Margin, RichText};

/// Why the app cannot do anything on this system, if so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    NotRoot,
    NotImmutable,
}

impl Blocked {
    /// Check the preconditions the whole UI depends on.
    pub fn detect() -> Option<Self> {
        if !nix::unistd::geteuid().is_root() {
            return Some(Self::NotRoot);
        }
        if Backend::detect().is_none() {
            return Some(Self::NotImmutable);
        }
        None
    }
}

pub struct UpdateGui {
    state: AppState,
    blocked: Option<Blocked>,
    refreshed_once: bool,
}

impl UpdateGui {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        style::apply(&cc.egui_ctx);
        Self {
            state: AppState::default(),
            blocked: Blocked::detect(),
            refreshed_once: false,
        }
    }

    fn show_blocked(&self, ui: &mut egui::Ui, reason: Blocked) {
        let (title, text, hint) = match reason {
            Blocked::NotRoot => (
                "Administrator access required",
                "Building a system update needs root. Launch this from the application menu, \
                 which asks for authentication, or run it with pkexec.",
                None,
            ),
            // The package is only installed on immutable deployments, so this
            // is the hand-installed or migrated case rather than the norm.
            Blocked::NotImmutable => (
                "Not an immutable system",
                "This tool drives deploytix's transactional updates, which only exist on an \
                 immutable install. This system has a writable root, so there are no \
                 snapshots to manage.",
                Some("Update this system with pacman as usual."),
            ),
        };
        ui.add_space(ui.available_height() * 0.25);
        ui.vertical_centered(|ui| {
            ui.set_max_width(520.0);
            ui::card(ui, "Deploytix Update", |ui| {
                ui.label(
                    RichText::new(title)
                        .font(style::medium(16.0))
                        .color(style::TEXT),
                );
                ui.add_space(6.0);
                ui.label(text);
                if let Some(hint) = hint {
                    ui.add_space(6.0);
                    ui::note(ui, hint);
                }
            });
        });
    }

    /// Title, what is booted and what boots next, and Refresh.
    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.set_min_height(30.0);
            ui.label(
                RichText::new("Deploytix Update")
                    .font(style::medium(15.0))
                    .color(style::TEXT),
            );
            if let Some(info) = &self.state.info {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(info.backend.label())
                        .size(style::SMALL)
                        .color(style::TEXT_FAINT),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let refreshing = self.state.refreshing;
                if ui::button(ui, !refreshing, "Refresh").clicked() {
                    self.state.start_refresh();
                }
                if refreshing {
                    ui.spinner();
                }
                ui.add_space(8.0);
                if let Some(info) = self.state.info.clone() {
                    let noun = info.backend.target_noun();
                    if info.has_staged_update() {
                        ui::badge(
                            ui,
                            &format!("Next boot: {}", describe_target(&info.pointer, noun)),
                            Tone::Warning,
                        )
                        .on_hover_text("An update is staged and takes effect on the next reboot.");
                    }
                    ui::badge(
                        ui,
                        &format!("Running: {}", describe_target(&info.running, noun)),
                        Tone::Success,
                    );
                }
            });
        });
    }

    fn tabs(&mut self, ui: &mut egui::Ui) {
        let state = &self.state;
        let dot = |tab: Tab| match tab {
            Tab::Progress if state.question.is_some() => Some(style::WARNING),
            Tab::Progress if state.busy => Some(style::ACCENT),
            Tab::Progress if state.error.is_some() => Some(style::ERROR),
            Tab::Aur => state
                .aur_plan
                .as_ref()
                .filter(|p| !p.outdated.is_empty())
                .map(|_| style::ACCENT),
            _ => None,
        };
        let items: Vec<ui::TabItem> = Tab::ALL
            .iter()
            .map(|&tab| ui::TabItem {
                label: tab.label(),
                selected: state.tab == tab,
                dot: dot(tab),
            })
            .collect();
        if let Some(i) = ui::tab_strip(ui, &items) {
            self.state.tab = Tab::ALL[i];
        }
    }

    /// What is running, and the two options every transaction takes.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let state = &mut self.state;
        ui.horizontal(|ui| {
            ui.set_min_height(28.0);
            if state.busy {
                ui.spinner();
                let status = match state.phase {
                    Some(phase) => phase.label().to_string(),
                    None => state.status.clone(),
                };
                ui.label(RichText::new(status).color(style::TEXT));
                if state.tab != Tab::Progress && ui::quiet(ui, true, "View progress").clicked() {
                    state.tab = Tab::Progress;
                }
            } else if let Some(err) = &state.error {
                ui::swatch(ui, style::ERROR);
                let first = err.lines().next().unwrap_or_default().to_string();
                ui.label(RichText::new(first).color(style::ERROR));
            } else if let Some(done) = &state.finished {
                ui::swatch(ui, style::SUCCESS);
                ui.label(RichText::new(done).color(style::SUCCESS));
            } else {
                ui::swatch(ui, style::BORDER_STRONG);
                ui.label(RichText::new("Ready").color(style::TEXT_FAINT));
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(concat!("deploytix ", env!("CARGO_PKG_VERSION")))
                        .size(style::SMALL)
                        .color(style::TEXT_FAINT),
                );
                ui.add_space(12.0);
                ui.add_enabled_ui(!state.busy, |ui| {
                    ui.checkbox(&mut state.reboot_after, "Reboot when done");
                    ui.add_space(12.0);
                    ui.add(
                        egui::DragValue::new(&mut state.keep_sets)
                            .range(1..=20)
                            .suffix(" snapshots"),
                    )
                    .on_hover_text(
                        "How many snapshots each transaction keeps; older ones are pruned.",
                    );
                    ui.label(RichText::new("Keep").color(style::TEXT_FAINT));
                });
            });
        });
    }
}

/// `@` and slot letters need expanding; a set id is dated in the Snapshots tab.
pub fn describe_target(target: &str, noun: &str) -> String {
    match target {
        "@" => "Base install".to_string(),
        "A" | "B" => format!("Slot {target}"),
        id => match id.parse::<u64>() {
            Ok(secs) => super::calendar::local_timestamp(secs),
            Err(_) => format!("{noun} {id}"),
        },
    }
}

fn bar_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(style::BAR)
        .inner_margin(Margin::symmetric(16, 0))
}

impl eframe::App for UpdateGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Windowed, so this tracks the window rather than the display: shrink
        // the window and the contents stay whole instead of being clipped.
        theme::fit_to_screen(ctx, theme::UPDATER_REFERENCE);

        if let Some(reason) = self.blocked {
            egui::CentralPanel::default().show(ctx, |ui| self.show_blocked(ui, reason));
            return;
        }

        // First frame: kick off the initial read of system state.
        if !self.refreshed_once {
            self.refreshed_once = true;
            self.state.start_refresh();
        }

        if self.state.pump() {
            ctx.request_repaint();
        }
        // pacman is stalled on a question; flash the window rather than wait
        // for the user to happen to look.
        if std::mem::take(&mut self.state.wants_attention) {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Informational,
            ));
        }
        // A running operation produces messages sporadically; keep the frame
        // loop alive so a finished worker is noticed promptly.
        if self.state.busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }

        egui::TopBottomPanel::top("header")
            .frame(bar_frame().inner_margin(Margin {
                left: 16,
                right: 16,
                top: 10,
                bottom: 0,
            }))
            .show(ctx, |ui| {
                self.header(ui);
                ui.add_space(4.0);
                self.tabs(ui);
            });

        egui::TopBottomPanel::bottom("status")
            .frame(bar_frame().inner_margin(Margin::symmetric(16, 4)))
            .show(ctx, |ui| self.status_bar(ui));

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(style::BG)
                    .inner_margin(Margin::same(16)),
            )
            .show(ctx, |ui| match self.state.tab {
                Tab::System => panels::system::show(ui, &mut self.state),
                Tab::Update => panels::update::show(ui, &mut self.state),
                Tab::Aur => panels::aur::show(ui, &mut self.state),
                Tab::Snapshots => panels::snapshots::show(ui, &mut self.state),
                Tab::Progress => panels::progress::show(ui, &mut self.state),
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This container has no immutable markers, so the app must refuse to
    /// present any controls on it. Mirrors the real guarantee: even if the
    /// package is somehow installed on a mutable system, the UI stands down.
    #[test]
    fn a_non_immutable_system_blocks_the_ui() {
        assert!(
            Blocked::detect().is_some(),
            "a system with no /.deploytix-pair and no slot state must be blocked"
        );
    }

    #[test]
    fn backend_detection_agrees_with_the_block_check() {
        // The two must never disagree, or the UI would render tabs it cannot
        // drive.
        if Backend::detect().is_none() {
            assert_eq!(
                Blocked::detect(),
                Some(if nix::unistd::geteuid().is_root() {
                    Blocked::NotImmutable
                } else {
                    Blocked::NotRoot
                })
            );
        }
    }
}
