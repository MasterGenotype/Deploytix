//! User configuration panel

use crate::gui::{state::UserState, theme, widgets};
use egui::Ui;

/// Render user configuration sections. Returns `true` when valid.
pub(crate) fn show_sections(ui: &mut Ui, user: &mut UserState) -> bool {
    widgets::section(ui, "Account", |ui| {
        widgets::text_row(ui, "Username:", &mut user.username);
        ui.add_space(theme::SPACING_XS);

        widgets::password_row(ui, "Password:", &mut user.password);
        ui.add_space(theme::SPACING_XS);

        widgets::password_row(ui, "Confirm:", &mut user.password_confirm);
        ui.add_space(theme::SPACING_SM);

        ui.checkbox(&mut user.sudoer, "Add user to wheel group (sudo access)");
    });

    // Validation
    if user.username.is_empty() {
        widgets::validation_error(ui, "Username cannot be empty");
        return false;
    }
    if user.username.contains(' ') {
        widgets::validation_error(ui, "Username cannot contain spaces");
        return false;
    }
    if user.password.is_empty() {
        widgets::validation_error(ui, "Password cannot be empty");
        return false;
    }
    if user.password != user.password_confirm {
        widgets::validation_error(ui, "Passwords do not match");
        return false;
    }

    true
}
