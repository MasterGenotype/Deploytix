//! XFCE.

use super::{DesktopModule, DesktopSession};
use crate::config::DesktopEnvironment;

/// XFCE packages (display manager handled centrally via desktop.display_manager)
const XFCE_PACKAGES: &[&str] = &["xfce4", "xfce4-goodies"];

pub const MODULE: DesktopModule = DesktopModule {
    id: DesktopEnvironment::Xfce,
    label: "XFCE",
    packages: XFCE_PACKAGES,
    service_packages: &[],
    xinitrc_command: Some("startxfce4"),
    session: Some(DesktopSession {
        command: "startxfce4",
        fallbacks: &["startplasma-wayland", "gnome-session"],
        procs: &[
            "x:startxfce4",
            "x:xfce4-session",
            "x:xfwm4",
            "x:xfdesktop",
            "x:xfce4-panel",
            "f:xdg-desktop-portal-xfce",
        ],
    }),
    sddm_conf: None,
    desktop_file: desktop_file_content,
};

/// Generate XFCE-specific desktop file content
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
Categories=System;Settings;XFCE;GTK;
Keywords=linux;installer;artix;deployment;xfce;
X-XFCE-Category=SystemSetup
"#,
        bindir
    )
}
