//! Desktop environment installers

use crate::config::DesktopEnvironment;

pub mod gnome;
pub mod kde;
pub mod none;
pub mod xfce;

/// Generate desktop file content based on the detected desktop environment
pub fn generate_desktop_file(de: &DesktopEnvironment, bindir: &str) -> String {
    match de {
        DesktopEnvironment::None => none::desktop_file_content(bindir),
        DesktopEnvironment::Kde => kde::desktop_file_content(bindir),
        DesktopEnvironment::Gnome => gnome::desktop_file_content(bindir),
        DesktopEnvironment::Xfce => xfce::desktop_file_content(bindir),
    }
}
