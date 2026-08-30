//! Unified configuration panel
//!
//! Displays all pre-install options as a responsive grid.  The number of
//! columns is derived from the available width (3 when there is room, then 2,
//! then 1) so the controls never get squeezed into unreadable slivers, and the
//! whole grid lives in a scroll area so no option is cut off when the content
//! is taller than the window — at any zoom level or window size.

use crate::gui::{
    state::{DiskState, PackagesState, SystemState, UserState},
    theme,
};
use egui::{RichText, Ui};

use super::{
    disk_config, disk_selection, handheld_gaming, network_desktop, system_config, user_config,
};

/// Logical groups of settings distributed across the grid columns.
const GROUP_COUNT: usize = 3;

/// Column heading rendered at the top of each grid column.
fn column_heading(ui: &mut Ui, title: &str) {
    ui.label(
        RichText::new(title)
            .strong()
            .size(16.0)
            .color(theme::ACCENT),
    );
    ui.separator();
    ui.add_space(theme::SPACING_XS);
}

/// Sub-heading used to separate logical groups within a column.
fn sub_heading(ui: &mut Ui, title: &str) {
    ui.add_space(theme::SPACING_SM);
    ui.label(
        RichText::new(title)
            .strong()
            .size(14.0)
            .color(theme::ACCENT),
    );
    ui.separator();
    ui.add_space(theme::SPACING_XS);
}

/// Heading for a group, styled as a column heading when it opens the column
/// and as a sub-heading when it is stacked below another group.
fn group_heading(ui: &mut Ui, title: &str, column_is_empty: &mut bool) {
    if *column_is_empty {
        column_heading(ui, title);
        *column_is_empty = false;
    } else {
        sub_heading(ui, title);
    }
}

/// How many columns fit in `available_width` without squeezing any of them
/// below [`theme::MIN_COLUMN_WIDTH`].
fn column_count(available_width: f32) -> usize {
    let fits = (available_width / theme::MIN_COLUMN_WIDTH).floor();
    (fits.max(1.0) as usize).clamp(1, GROUP_COUNT)
}

/// Render the unified configuration panel.
///
/// Returns `true` when all sections pass validation and the user may
/// proceed to the review step.
pub fn show(
    ui: &mut Ui,
    disk: &mut DiskState,
    system: &mut SystemState,
    user: &mut UserState,
    packages: &mut PackagesState,
) -> bool {
    let mut disk_selected = false;
    let mut disk_valid = false;
    let mut system_valid = false;
    let mut user_valid = false;

    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            // Measured inside the scroll area so the space the scrollbar
            // reserves is accounted for.
            let columns = column_count(ui.available_width());

            ui.columns(columns, |cols| {
                // Groups are spread evenly over however many columns we have:
                // 3 columns → one group each; 2 → Disk+System share the first;
                // 1 → everything stacks and the scroll area takes over.
                let column_of = |group: usize| group * columns / GROUP_COUNT;
                let mut empty = vec![true; columns];

                // ═══ Group 1: Disk ════════════════════════════════════════
                let c = column_of(0);
                group_heading(&mut cols[c], "Disk", &mut empty[c]);
                disk_selected = disk_selection::show_sections(&mut cols[c], disk);
                cols[c].add_space(theme::SPACING_SM);
                disk_valid = disk_config::show_sections(&mut cols[c], disk);

                // ═══ Group 2: System & User ═══════════════════════════════
                let c = column_of(1);
                group_heading(&mut cols[c], "System", &mut empty[c]);
                system_valid = system_config::show_sections(&mut cols[c], system);
                sub_heading(&mut cols[c], "User Account");
                user_valid = user_config::show_sections(&mut cols[c], user);

                // ═══ Group 3: Packages & Gaming ═══════════════════════════
                let c = column_of(2);
                group_heading(&mut cols[c], "Packages", &mut empty[c]);
                network_desktop::show_sections(
                    &mut cols[c],
                    packages,
                    &disk.filesystem,
                    disk.use_lvm_thin,
                );
                sub_heading(&mut cols[c], "Gaming");
                handheld_gaming::show_sections(&mut cols[c], packages);
            });
        });

    disk_selected && disk_valid && system_valid && user_valid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_count_shrinks_with_width() {
        assert_eq!(column_count(1600.0), 3);
        assert_eq!(column_count(3.0 * theme::MIN_COLUMN_WIDTH), 3);
        assert_eq!(column_count(3.0 * theme::MIN_COLUMN_WIDTH - 1.0), 2);
        assert_eq!(column_count(2.0 * theme::MIN_COLUMN_WIDTH), 2);
        assert_eq!(column_count(theme::MIN_COLUMN_WIDTH), 1);
        assert_eq!(column_count(0.0), 1);
    }

    #[test]
    fn every_group_lands_in_a_real_column() {
        for columns in 1..=GROUP_COUNT {
            for group in 0..GROUP_COUNT {
                assert!(group * columns / GROUP_COUNT < columns);
            }
        }
    }

    #[test]
    fn three_columns_keep_one_group_each() {
        let columns = 3;
        let assigned: Vec<usize> = (0..GROUP_COUNT)
            .map(|g| g * columns / GROUP_COUNT)
            .collect();
        assert_eq!(assigned, vec![0, 1, 2]);
    }
}
