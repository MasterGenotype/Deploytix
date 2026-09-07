//! Network and desktop configuration panel

use crate::config::{
    DesktopEnvironment, DisplayManager, Filesystem, IwdFrontend, NetworkBackend, TkgScheduler,
};
use crate::gui::{state::PackagesState, theme, widgets};
use egui::Ui;

/// Render network & desktop sections.
pub(crate) fn show_sections(
    ui: &mut Ui,
    packages: &mut PackagesState,
    filesystem: &Filesystem,
    use_lvm_thin: bool,
) {
    widgets::section(ui, "Network", |ui| {
        // Steam's gamepad UI configures Wi-Fi through NetworkManager; the
        // standalone iwd backend would leave Game Mode network setup broken
        // (and fail validation), so coerce it while session switching is on.
        if packages.install_session_switching && packages.network_backend == NetworkBackend::Iwd {
            packages.network_backend = NetworkBackend::NetworkManager;
        }
        ui.horizontal(|ui| {
            ui.label("Backend:");
            egui::ComboBox::from_id_salt("network")
                .selected_text(format!("{}", packages.network_backend))
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut packages.network_backend,
                        NetworkBackend::Iwd,
                        "iwd + GUI frontend (AUR)",
                    );
                    ui.selectable_value(
                        &mut packages.network_backend,
                        NetworkBackend::NetworkManager,
                        "NetworkManager + iwd",
                    );
                    ui.selectable_value(
                        &mut packages.network_backend,
                        NetworkBackend::NetworkManagerWpa,
                        "NetworkManager + wpa_supplicant",
                    );
                });
        });
        if packages.install_session_switching {
            widgets::info_text(
                ui,
                "Game Mode session switching requires a NetworkManager backend \
                 (Steam's gamepad UI configures Wi-Fi through NetworkManager).",
            );
        }

        // Sub-choice: iwd GUI frontend (AUR) only when standalone iwd is picked.
        if packages.network_backend == NetworkBackend::Iwd {
            ui.add_space(theme::SPACING_XS);
            ui.horizontal(|ui| {
                ui.label("Frontend:");
                egui::ComboBox::from_id_salt("iwd_frontend")
                    .selected_text(format!("{}", packages.iwd_frontend))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut packages.iwd_frontend,
                            IwdFrontend::Iwgtk,
                            "iwgtk (GTK)",
                        );
                        ui.selectable_value(
                            &mut packages.iwd_frontend,
                            IwdFrontend::Iwdgui,
                            "iwdgui (GTK)",
                        );
                        ui.selectable_value(
                            &mut packages.iwd_frontend,
                            IwdFrontend::Iwqt,
                            "iwqt (Qt)",
                        );
                    });
            });
            if !packages.install_yay {
                widgets::info_text(
                    ui,
                    "Requires: yay AUR helper (enable in Optional Packages below). \
                     Validation will fail without it.",
                );
            }
        }

        ui.add_space(theme::SPACING_XS);

        // Optional Wi-Fi pre-seeding — gives the installed system connectivity
        // from the very first boot (Steam's first-run bootstrap in Game Mode
        // needs network before its own OOBE network page exists).
        ui.label("Pre-seed Wi-Fi network (optional):");
        ui.horizontal(|ui| {
            ui.label("SSID:");
            ui.text_edit_singleline(&mut packages.wifi_ssid);
        });
        if !packages.wifi_ssid.is_empty() {
            ui.horizontal(|ui| {
                ui.label("Passphrase:");
                ui.add(egui::TextEdit::singleline(&mut packages.wifi_password).password(true));
            });
            widgets::info_text(
                ui,
                "Credentials are written to the installed system so it auto-connects \
                 on first boot. Leave the passphrase empty for an open network.",
            );
        } else if packages.install_session_switching {
            // Mirrors DeploymentConfig::warnings(): Steam's gamepad UI is drawn
            // by steamwebhelper, which cannot start on a never-signed-in client
            // with no network — so the OOBE page that would let the user set up
            // Wi-Fi never appears.
            widgets::info_text(
                ui,
                "\u{26a0} Game Mode is enabled with no Wi-Fi pre-seeded. Steam's gamepad \
                 UI needs network access on first boot before it can show its own \
                 network-setup page, so a machine with no wired connection may reach \
                 Game Mode unable to get online.",
            );
        }

        ui.add_space(theme::SPACING_XS);

        ui.checkbox(
            &mut packages.sysctl_network_performance,
            "Network performance sysctl tweaks (BBR + fq, larger buffers, ECN\u{2026})",
        );
        if packages.sysctl_network_performance {
            widgets::info_text(
                ui,
                "Writes /etc/sysctl.d/99-network-performance.conf. Switches TCP \
                 congestion control to BBR and raises socket buffer ceilings for \
                 Wi-Fi 6 / 1\u{00a0}GbE+ links.",
            );
        }
    });

    widgets::section(ui, "Desktop Environment", |ui| {
        egui::ComboBox::from_id_salt("desktop")
            .selected_text(format!("{}", packages.desktop_env))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut packages.desktop_env,
                    DesktopEnvironment::None,
                    "None (headless/server)",
                );
                ui.selectable_value(
                    &mut packages.desktop_env,
                    DesktopEnvironment::Kde,
                    "KDE Plasma",
                );
                ui.selectable_value(
                    &mut packages.desktop_env,
                    DesktopEnvironment::Gnome,
                    "GNOME",
                );
                ui.selectable_value(&mut packages.desktop_env, DesktopEnvironment::Xfce, "XFCE");
            });

        if packages.desktop_env != DesktopEnvironment::None {
            // The gamescope ↔ desktop loop is built on greetd; coerce the
            // display manager while session switching is on (mirrors the
            // network backend coercion above and the config validation).
            if packages.install_session_switching
                && packages.display_manager != DisplayManager::Greetd
            {
                packages.display_manager = DisplayManager::Greetd;
            }

            ui.add_space(theme::SPACING_XS);
            ui.horizontal(|ui| {
                ui.label("Display manager:");
                egui::ComboBox::from_id_salt("display_manager")
                    .selected_text(format!("{}", packages.display_manager))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut packages.display_manager,
                            DisplayManager::Greetd,
                            "greetd (auto-login, deploytix default)",
                        );
                        ui.selectable_value(
                            &mut packages.display_manager,
                            DisplayManager::Sddm,
                            "SDDM (login screen)",
                        );
                        ui.selectable_value(
                            &mut packages.display_manager,
                            DisplayManager::Gdm,
                            "GDM (login screen)",
                        );
                        ui.selectable_value(
                            &mut packages.display_manager,
                            DisplayManager::Lightdm,
                            "LightDM (login screen)",
                        );
                        ui.selectable_value(
                            &mut packages.display_manager,
                            DisplayManager::None,
                            "None (TTY login, startx)",
                        );
                    });
            });
            if packages.install_session_switching {
                widgets::info_text(
                    ui,
                    "Game Mode session switching is driven through greetd, \
                     so the display manager is locked to greetd.",
                );
            } else if packages.display_manager == DisplayManager::Greetd {
                widgets::info_text(
                    ui,
                    "greetd auto-logins your user straight into the desktop \
                     session on boot (no login screen).",
                );
            } else if packages.display_manager == DisplayManager::None {
                widgets::info_text(
                    ui,
                    "The system boots to a TTY login; start the desktop \
                     manually with startx (~/.xinitrc is set up).",
                );
            }
        }
    });

    widgets::section(ui, "GPU Drivers", |ui| {
        ui.checkbox(
            &mut packages.gpu_nvidia,
            "NVIDIA (nvidia, nvidia-utils, linux-firmware-nvidia)",
        );
        ui.checkbox(
            &mut packages.gpu_amd,
            "AMD (mesa, vulkan-radeon, xf86-video-amdgpu)",
        );
        ui.checkbox(
            &mut packages.gpu_intel,
            "Intel (mesa, vulkan-intel, xf86-video-intel)",
        );
    });

    widgets::section(ui, "Kernel", |ui| {
        ui.checkbox(
            &mut packages.install_tkg_kernel,
            "linux-tkg (prebuilt, replaces linux-zen)",
        );
        if packages.install_tkg_kernel {
            ui.add_space(theme::SPACING_XS);
            ui.horizontal(|ui| {
                ui.label("CPU scheduler");
                egui::ComboBox::from_id_salt("tkg_scheduler")
                    .selected_text(format!("{}", packages.tkg_scheduler))
                    .show_ui(ui, |ui| {
                        for sched in TkgScheduler::all() {
                            ui.selectable_value(
                                &mut packages.tkg_scheduler,
                                *sched,
                                format!("{sched}"),
                            );
                        }
                    });
            });
            widgets::info_text(
                ui,
                "Frogging-Family publish prebuilt Arch packages for each release, so nothing \
                 is compiled here: the newest kernel and headers are downloaded from GitHub \
                 and installed with pacman -U.",
            );
            // Not a footnote.  This is the only kernel the target gets, so a
            // kernel that will not boot on this hardware has no menu entry to
            // fall back to, and the install aborts rather than continuing if
            // the download fails.
            widgets::validation_warning(
                ui,
                "This replaces linux-zen entirely — there is no fallback kernel in the boot \
                 menu. The install will stop if the download fails.",
            );
            if *filesystem == Filesystem::Zfs {
                widgets::validation_error(
                    ui,
                    "ZFS needs a matching kernel module and there is no zfs-linux-tkg. Pick \
                     another filesystem or untick linux-tkg.",
                );
            }
            if packages.gpu_nvidia {
                widgets::info_text(
                    ui,
                    "NVIDIA will be installed as nvidia-dkms: the prebuilt nvidia module is \
                     tied to a stock kernel and will not load on linux-tkg.",
                );
            }
        }
    });

    widgets::section(ui, "Optional Packages", |ui| {
        ui.checkbox(
            &mut packages.install_wine,
            "Wine compatibility (wine, vkd3d, winetricks, wine-mono, wine-gecko)",
        );
        ui.add_space(theme::SPACING_XS);

        ui.checkbox(
            &mut packages.install_warp_terminal,
            "Warp Terminal: The Agentic Terminal",
        );
        if packages.install_warp_terminal {
            widgets::info_text(
                ui,
                "Warp is in no repository and has no AUR package, so its Arch build is \
                 downloaded from warp.dev and installed with pacman -U.",
            );
        }
        ui.add_space(theme::SPACING_XS);

        ui.checkbox(
            &mut packages.install_yay,
            "yay AUR helper (built from source)",
        );
        if packages.install_yay {
            widgets::info_text(
                ui,
                "Go will be installed as a build dependency. yay is built as your user via makepkg.",
            );
        }
        ui.add_space(theme::SPACING_XS);

        // AUR package: only offered when there is a helper to build it, and
        // forced off otherwise so a stale config cannot ask for the impossible.
        if packages.install_yay {
            ui.checkbox(
                &mut packages.install_zen_browser,
                "Zen Browser (AUR: zen-browser-bin)",
            );
            ui.add_space(theme::SPACING_XS);
        } else {
            packages.install_zen_browser = false;
        }

        if packages.install_yay && *filesystem == Filesystem::Btrfs {
            ui.checkbox(
                &mut packages.install_btrfs_tools,
                "Btrfs snapshot tools (snapper, btrfs-assistant) via yay",
            );
            ui.add_space(theme::SPACING_XS);
        } else {
            packages.install_btrfs_tools = false;
        }

        if *filesystem == Filesystem::Btrfs {
            ui.checkbox(
                &mut packages.install_grub_btrfs,
                "grub-btrfs (bootable snapshot menu entries)",
            );
            if packages.install_grub_btrfs {
                widgets::info_text(
                    ui,
                    "Installs grub-btrfs + snapper, creates a @snapshots subvolume, and \
                     enables the grub-btrfsd daemon. Not compatible with LVM thin.",
                );
            }
            ui.add_space(theme::SPACING_XS);

            // Transactional immutable root builds on the grub-btrfs snapshot
            // machinery, so it is only offered when grub-btrfs is enabled.
            if packages.install_grub_btrfs {
                ui.checkbox(
                    &mut packages.immutable_root,
                    "Transactional immutable root (read-only /usr + /)",
                );
                if packages.immutable_root {
                    widgets::info_text(
                        ui,
                        "Mounts / and /usr read-only, keeps /etc on a writable @etc \
                         subvolume, and snapshots {@, @usr, @etc} as atomic sets. \
                         Updates go through `deploytix update` (a new snapshot set \
                         applied on reboot); direct `pacman -Syu` is blocked.",
                    );
                }
                ui.add_space(theme::SPACING_XS);
            } else {
                packages.immutable_root = false;
            }
        } else {
            packages.install_grub_btrfs = false;
            if !use_lvm_thin {
                packages.immutable_root = false;
            }
        }

        // LVM thin backend: A/B dual-slot dm-verity immutable root. Offered when
        // LVM thin is selected (grub-btrfs is not available on that layout).
        if use_lvm_thin {
            ui.checkbox(
                &mut packages.immutable_root,
                "Transactional immutable root (A/B dual-slot, dm-verity read-only /)",
            );
            if packages.immutable_root {
                widgets::info_text(
                    ui,
                    "Two root LVs (A/B); each slot's root (including /usr) is mounted \
                     read-only and dm-verity integrity-checked. `deploytix update` builds \
                     the inactive slot and flips the boot pointer on reboot; \
                     `deploytix rollback` flips back. Direct `pacman -Syu` is blocked.",
                );
            }
            ui.add_space(theme::SPACING_XS);
        } else if *filesystem != Filesystem::Btrfs {
            // Neither backend applies.
            packages.immutable_root = false;
        }
    });

    widgets::section(ui, "Extra Packages", |ui| {
        widgets::info_text(
            ui,
            "Anything else to install, separated by spaces, commas or newlines. \
             These are installed at the end of the run, after the base system.",
        );
        ui.add_space(theme::SPACING_XS);

        ui.label("Repository packages (pacman -S)");
        ui.add(
            egui::TextEdit::multiline(&mut packages.extra_pacman)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .hint_text("neovim htop ripgrep"),
        );
        ui.add_space(theme::SPACING_XS);

        ui.label("AUR packages (yay -S)");
        ui.add(
            egui::TextEdit::multiline(&mut packages.extra_aur)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .hint_text("visual-studio-code-bin"),
        );
        // Validation rejects AUR extras without yay, so say so here rather
        // than letting the install fail at the summary step.
        if !crate::gui::state::split_package_list(&packages.extra_aur).is_empty()
            && !packages.install_yay
        {
            widgets::validation_error(
                ui,
                "AUR packages need the yay AUR helper. Tick it under Optional Packages.",
            );
        }
    });
}
