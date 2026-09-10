//! KDE Plasma.

use super::{DesktopModule, DesktopSession};
use crate::config::DesktopEnvironment;

/// KDE Plasma packages (individual packages instead of plasma-meta to avoid systemd conflicts on Artix)
const KDE_PACKAGES: &[&str] = &[
    "plasma-desktop",
    "plasma-workspace",
    "konsole",
    "dolphin",
    // KDE audio integration
    "plasma-pa",
    "kpipewire",
    // Desktop integration
    "bluez",
    "power-profiles-daemon",
    // KDE system integration
    "powerdevil",
    "bluedevil",
    "kde-gtk-config",
    "kdeplasma-addons",
    "kscreen",
    "kwallet-pam",
    "xdg-desktop-portal-kde",
    // Application store
    "discover",
    "flatpak",
    "kate",
];

/// Services KDE expects, installed as `{name}-{init}` alongside the base
/// packages above.
const KDE_SERVICE_PACKAGES: &[&str] = &["bluez", "power-profiles-daemon"];

pub const MODULE: DesktopModule = DesktopModule {
    id: DesktopEnvironment::Kde,
    label: "KDE Plasma",
    packages: KDE_PACKAGES,
    service_packages: KDE_SERVICE_PACKAGES,
    xinitrc_command: Some("startplasma-x11"),
    session: Some(DesktopSession {
        command: "startplasma-wayland",
        fallbacks: &["gnome-session", "startxfce4"],
        procs: &[
            "x:startplasma-wayland",
            "x:plasma_session",
            "x:kwin_wayland",
            "x:kwin_wayland_wrapper",
            "x:kded6",
            "f:kactivitymanagerd",
            "f:xdg-desktop-portal-kde",
        ],
    }),
    sddm_conf: Some("[Theme]\nCurrent=breeze\n\n"),
    desktop_file: desktop_file_content,
};

/// Generate KDE-specific desktop file content
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
Categories=System;Settings;Qt;KDE;
Keywords=linux;installer;artix;deployment;kde;plasma;
X-KDE-SubstituteUID=false
X-DBUS-StartupType=
X-KDE-StartupNotify=true
"#,
        bindir
    )
}
