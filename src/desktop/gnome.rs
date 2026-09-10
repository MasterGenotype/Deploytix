//! GNOME.

use super::{DesktopModule, DesktopSession};
use crate::config::DesktopEnvironment;

/// GNOME packages (display manager handled centrally via desktop.display_manager)
const GNOME_PACKAGES: &[&str] = &["gnome", "gnome-extra"];

pub const MODULE: DesktopModule = DesktopModule {
    id: DesktopEnvironment::Gnome,
    label: "GNOME",
    packages: GNOME_PACKAGES,
    service_packages: &[],
    xinitrc_command: Some("gnome-session"),
    session: Some(DesktopSession {
        command: "gnome-session",
        fallbacks: &["startplasma-wayland", "startxfce4"],
        procs: &[
            "x:gnome-session",
            "x:gnome-session-binary",
            "x:gnome-shell",
            "x:gsd-media-keys",
            "f:xdg-desktop-portal-gnome",
        ],
    }),
    sddm_conf: None,
    desktop_file: desktop_file_content,
};

/// Generate GNOME-specific desktop file content
pub fn desktop_file_content(bindir: &str) -> String {
    format!(
        r#"[Desktop Entry]
Type=Application
Name=Deploytix
GenericName=Artix Linux Installer
Comment=Automated Artix Linux deployment installer
Exec=pkexec {}/deploytix-gui
Icon=system-software-install
NoDisplay=false
StartupNotify=true
Terminal=false
Categories=System;Settings;GNOME;GTK;
Keywords=linux;installer;artix;deployment;gnome;
X-GNOME-UsesNotifications=true
X-GNOME-Autostart-Phase=Application
"#,
        bindir
    )
}
