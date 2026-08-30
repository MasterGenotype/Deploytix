//! The update GUI application shell.

use super::model::Backend;
use super::panels;
use super::state::{AppState, Tab};
use crate::gui::{theme, widgets};
use eframe::egui;
use egui::RichText;

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
        theme::apply(&cc.egui_ctx);
        Self {
            state: AppState::default(),
            blocked: Blocked::detect(),
            refreshed_once: false,
        }
    }

    fn show_blocked(&self, ui: &mut egui::Ui, reason: Blocked) {
        widgets::page_heading(ui, "Deploytix Update");
        match reason {
            Blocked::NotRoot => {
                widgets::section(ui, "Administrator access required", |ui| {
                    ui.label(
                        RichText::new(
                            "Building a system update needs root. Launch this from the \
                             application menu, which asks for authentication, or run it with \
                             pkexec.",
                        )
                        .color(theme::TEXT_SECONDARY),
                    );
                });
            }
            // The package is only installed on immutable deployments, so this
            // is the hand-installed or migrated case rather than the norm.
            Blocked::NotImmutable => {
                widgets::section(ui, "Not an immutable system", |ui| {
                    ui.label(
                        RichText::new(
                            "This tool drives deploytix's transactional updates, which only \
                             exist on an immutable install. This system has a writable root, \
                             so there are no snapshots to manage.",
                        )
                        .color(theme::TEXT_SECONDARY),
                    );
                    ui.add_space(theme::SPACING_SM);
                    ui.label(
                        RichText::new("Update this system with pacman as usual.")
                            .color(theme::TEXT_MUTED),
                    );
                });
            }
        }
    }
}

impl eframe::App for UpdateGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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
        // A running operation produces messages sporadically; keep the frame
        // loop alive so a finished worker is noticed promptly.
        if self.state.busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.add_space(theme::SPACING_XS);
            ui.horizontal(|ui| {
                for tab in Tab::ALL {
                    let selected = self.state.tab == tab;
                    if ui.selectable_label(selected, tab.label()).clicked() {
                        self.state.tab = tab;
                    }
                }
            });
            ui.add_space(theme::SPACING_XS);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match self.state.tab {
                    Tab::System => panels::system::show(ui, &mut self.state),
                    Tab::Update => panels::update::show(ui, &mut self.state),
                    Tab::Snapshots => panels::snapshots::show(ui, &mut self.state),
                    Tab::Progress => panels::progress::show(ui, &mut self.state),
                });
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
