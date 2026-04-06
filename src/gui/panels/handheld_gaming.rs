//! Handheld gaming device configuration panel

use crate::config::DesktopEnvironment;
use crate::gui::{state::PackagesState, theme, widgets};
use egui::Ui;

/// Render the handheld gaming panel. Always returns `true`.
pub fn show(ui: &mut Ui, packages: &mut PackagesState) -> bool {
    widgets::page_heading(ui, "Handheld Gaming Device");

    widgets::validation_warning(
        ui,
        "This section is for handheld gaming devices (e.g. Steam Deck, GPD, AYANEO). \
         These features may not work correctly on non-gaming hardware.",
    );
    ui.add_space(theme::SPACING_MD);

    widgets::section(ui, "Gaming", |ui| {
        ui.checkbox(
            &mut packages.install_gaming,
            "Gaming packages (Steam, gamescope)",
        );

        if packages.install_gaming && packages.desktop_env != DesktopEnvironment::None {
            ui.add_space(theme::SPACING_XS);
            ui.indent("session_switching_indent", |ui| {
                ui.checkbox(
                    &mut packages.install_session_switching,
                    "Game Mode \u{2194} Desktop switching",
                );
                if packages.install_session_switching {
                    widgets::info_text(
                        ui,
                        "Boots into gamescope Game Mode; switch to desktop and back on demand.",
                    );
                }
            });
        } else {
            packages.install_session_switching = false;
        }
    });

    true
}
