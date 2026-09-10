//! Headless/server: no desktop environment.

use super::DesktopModule;
use crate::config::DesktopEnvironment;

/// The absence of a desktop, expressed as a module rather than as a special
/// case: no packages, no session, nothing to start. Every `is_graphical()`
/// check downstream keys off this being empty.
pub const MODULE: DesktopModule = DesktopModule {
    id: DesktopEnvironment::None,
    label: "None (headless/server)",
    packages: &[],
    service_packages: &[],
    xinitrc_command: None,
    session: None,
    sddm_conf: None,
    desktop_file: desktop_file_content,
};

/// Generate generic desktop file content (no DE-specific features)
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
Categories=System;Settings;
Keywords=linux;installer;artix;deployment;
"#,
        bindir
    )
}
