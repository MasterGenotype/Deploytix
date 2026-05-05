//! Main GUI application

use crate::config::{
    DeploymentConfig, DesktopConfig, DiskConfig, GpuDriverVendor, NetworkConfig, PackagesConfig,
    SystemConfig, UserConfig,
};
use crate::disk::detection::list_block_devices;
use crate::install::Installer;
use eframe::egui;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use super::{panels, state::*, theme, widgets};

/// Main GUI application state.
pub struct DeploytixGui {
    step: WizardStep,
    disk: DiskState,
    system: SystemState,
    user: UserState,
    packages: PackagesState,
    install: InstallState,
    /// Tracks whether the configure panel passes validation (one-frame lag
    /// is fine in immediate-mode UI).
    config_valid: bool,
}

impl Default for DeploytixGui {
    fn default() -> Self {
        Self {
            step: WizardStep::Configure,
            disk: DiskState::default(),
            system: SystemState::default(),
            user: UserState::default(),
            packages: PackagesState::default(),
            install: InstallState::default(),
            config_valid: false,
        }
    }
}

impl DeploytixGui {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        Self::default()
    }

    fn refresh_disks(&mut self) {
        match list_block_devices(false) {
            Ok(devices) => {
                self.disk.devices = devices;
                self.disk.selected_device_index = None;
            }
            Err(e) => {
                self.disk.devices = Vec::new();
                self.install
                    .logs
                    .push(format!("Error listing disks: {}", e));
            }
        }
        self.disk.refreshing = false;
    }

    fn build_config(&self) -> DeploymentConfig {
        let device_path = self.disk.selected_device_path().to_string();

        DeploymentConfig {
            disk: DiskConfig {
                device: device_path,
                filesystem: self.disk.filesystem.clone(),
                boot_filesystem: crate::config::boot_filesystem_for(&self.disk.filesystem),
                encryption: self.disk.encryption,
                encryption_password: if self.disk.encryption {
                    Some(self.disk.encryption_password.clone())
                } else {
                    None
                },
                luks_mapper_name: crate::config::default_luks_mapper_name(),
                boot_encryption: self.disk.boot_encryption,
                luks_boot_mapper_name: crate::config::default_luks_boot_mapper_name(),
                keyfile_path: None,
                integrity: self.disk.integrity,
                keyfile_enabled: self.disk.encryption,
                use_subvolumes: self.disk.use_subvolumes,
                use_lvm_thin: self.disk.use_lvm_thin,
                lvm_vg_name: self.disk.lvm_vg_name.clone(),
                lvm_thin_pool_name: self.disk.lvm_thin_pool_name.clone(),
                lvm_thin_pool_percent: self.disk.lvm_thin_pool_percent,
                swap_type: self.disk.swap_type.clone(),
                swap_file_size_mib: 0,
                zram_algorithm: "zstd".to_string(),
                preserve_home: self.disk.preserve_home,
                partitions: self.disk.partitions.clone(),
            },
            system: SystemConfig {
                init: self.system.init_system.clone(),
                bootloader: self.system.bootloader.clone(),
                timezone: self.system.timezone.clone(),
                locale: self.system.locale.clone(),
                keymap: self.system.keymap.clone(),
                hostname: self.system.hostname.clone(),
                hibernation: false,
                secureboot: self.system.secureboot,
                secureboot_method: self.system.secureboot_method.clone(),
                secureboot_keys_path: None,
            },
            user: UserConfig {
                name: self.user.username.clone(),
                password: self.user.password.clone(),
                groups: crate::config::default_groups(),
                sudoer: self.user.sudoer,
            },
            network: NetworkConfig {
                backend: self.packages.network_backend.clone(),
            },
            desktop: DesktopConfig {
                environment: self.packages.desktop_env.clone(),
                display_manager: None,
            },
            packages: PackagesConfig {
                install_yay: self.packages.install_yay,
                install_wine: self.packages.install_wine,
                install_gaming: self.packages.install_gaming,
                install_session_switching: self.packages.install_session_switching,
                install_btrfs_tools: self.packages.install_btrfs_tools,
                install_modular: self.packages.install_modular,
                sysctl_gaming_tweaks: self.packages.sysctl_gaming_tweaks,
                sysctl_network_performance: self.packages.sysctl_network_performance,
                install_hhd: self.packages.install_hhd,
                install_decky_loader: self.packages.install_decky_loader,
                install_evdevhook2: self.packages.install_evdevhook2,
                gpu_drivers: {
                    let mut drivers = Vec::new();
                    if self.packages.gpu_nvidia {
                        drivers.push(GpuDriverVendor::Nvidia);
                    }
                    if self.packages.gpu_amd {
                        drivers.push(GpuDriverVendor::Amd);
                    }
                    if self.packages.gpu_intel {
                        drivers.push(GpuDriverVendor::Intel);
                    }
                    drivers
                },
            },
        }
    }

    fn save_config(&mut self) {
        let config = self.build_config();
        match toml::to_string_pretty(&config) {
            Ok(content) => match std::fs::write(&self.install.save_config_path, &content) {
                Ok(()) => {
                    self.install.save_config_status = Some((
                        format!("\u{2713} Saved to {}", self.install.save_config_path),
                        false,
                    ));
                }
                Err(e) => {
                    self.install.save_config_status =
                        Some((format!("\u{2717} Write failed: {}", e), true));
                }
            },
            Err(e) => {
                self.install.save_config_status =
                    Some((format!("\u{2717} Serialization failed: {}", e), true));
            }
        }
    }

    fn start_preflight(&mut self) {
        let config = self.build_config();
        self.install.preflight_running = true;
        self.install.preflight_results = None;
        self.install.preflight_has_failures = false;

        let (tx, rx): (Sender<InstallMessage>, Receiver<InstallMessage>) = channel();
        self.install.receiver = Some(rx);

        thread::spawn(move || {
            let report = crate::preflight::run_preflight(&config);
            let lines = report.to_log_lines();
            let has_failures = report.has_failures();
            let _ = tx.send(InstallMessage::PreflightResults {
                lines,
                has_failures,
            });
        });
    }

    fn start_rehearsal(&mut self) {
        let config = self.build_config();
        self.install.rehearsal_running = true;
        self.install.rehearsal_results = None;
        self.install.rehearsal_has_failures = false;

        let (tx, rx): (Sender<InstallMessage>, Receiver<InstallMessage>) = channel();
        self.install.receiver = Some(rx);

        thread::spawn(move || {
            let report = crate::rehearsal::run_rehearsal(&config);
            let lines = report.to_log_lines();
            let has_failures = report.has_failures();
            let _ = tx.send(InstallMessage::RehearsalResults {
                lines,
                has_failures,
            });
        });
    }

    fn start_installation(&mut self) {
        let config = self.build_config();
        let dry_run = self.install.dry_run;

        let (tx, rx): (Sender<InstallMessage>, Receiver<InstallMessage>) = channel();
        self.install.receiver = Some(rx);
        self.install.status = "Starting installation...".to_string();
        self.install.progress = 0.0;
        self.install.logs.clear();

        thread::spawn(move || {
            let _ = tx.send(InstallMessage::Log(format!(
                "Starting {} installation on {}",
                if dry_run { "dry-run" } else { "real" },
                config.disk.device
            )));
            let _ = tx.send(InstallMessage::Progress(0.05));

            if let Err(e) = config.validate() {
                let _ = tx.send(InstallMessage::Error(format!("Configuration error: {}", e)));
                return;
            }

            let _ = tx.send(InstallMessage::Log("Configuration validated".to_string()));
            let _ = tx.send(InstallMessage::Progress(0.1));

            if !dry_run && !nix::unistd::geteuid().is_root() {
                let _ = tx.send(InstallMessage::Error(
                    "Must run as root for real installation".to_string(),
                ));
                return;
            }

            let _ = tx.send(InstallMessage::Status("Running installer...".to_string()));
            let _ = tx.send(InstallMessage::Progress(0.15));

            let progress_tx = tx.clone();
            let progress_cb: crate::install::ProgressCallback =
                Box::new(move |progress, status| {
                    let gui_progress = 0.15 + progress * 0.80;
                    let _ = progress_tx.send(InstallMessage::Progress(gui_progress));
                    let _ = progress_tx.send(InstallMessage::Status(status.to_string()));
                    let _ = progress_tx.send(InstallMessage::Log(format!(
                        "[{:.0}%] {}",
                        gui_progress * 100.0,
                        status
                    )));
                });

            let installer = Installer::new(config, dry_run)
                .with_skip_confirm(true)
                .with_progress_callback(progress_cb);
            match installer.run() {
                Ok(()) => {
                    let _ = tx.send(InstallMessage::Progress(1.0));
                    let _ = tx.send(InstallMessage::Log(
                        "Installation completed successfully!".to_string(),
                    ));
                    let _ = tx.send(InstallMessage::Finished);
                }
                Err(e) => {
                    let _ = tx.send(InstallMessage::Error(format!("Installation failed: {}", e)));
                }
            }
        });
    }

    fn poll_install_messages(&mut self) {
        let mut should_clear = false;

        if let Some(ref rx) = self.install.receiver {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    InstallMessage::Status(s) => self.install.status = s,
                    InstallMessage::Progress(p) => self.install.progress = p,
                    InstallMessage::Log(s) => self.install.logs.push(s),
                    InstallMessage::Finished => {
                        self.install.finished = true;
                        should_clear = true;
                    }
                    InstallMessage::Error(e) => {
                        self.install.error = Some(e);
                        self.install.finished = true;
                        should_clear = true;
                    }
                    InstallMessage::PreflightResults {
                        lines,
                        has_failures,
                    } => {
                        self.install.preflight_results = Some(lines);
                        self.install.preflight_has_failures = has_failures;
                        self.install.preflight_running = false;
                        should_clear = true;
                    }
                    InstallMessage::RehearsalResults {
                        lines,
                        has_failures,
                    } => {
                        self.install.rehearsal_results = Some(lines);
                        self.install.rehearsal_has_failures = has_failures;
                        self.install.rehearsal_running = false;
                        should_clear = true;
                    }
                }
            }
        }

        if should_clear {
            self.install.receiver = None;
        }
    }
}

impl eframe::App for DeploytixGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.disk.refreshing {
            self.refresh_disks();
        }

        if self.install.receiver.is_some() {
            self.poll_install_messages();
            ctx.request_repaint();
        }

        // ── Header with step indicator ─────────────────────────────
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(theme::SPACING_SM);
            ui.horizontal(|ui| {
                ui.add_space(theme::SPACING_MD);
                ui.label(
                    egui::RichText::new("Deploytix")
                        .strong()
                        .size(18.0)
                        .color(theme::ACCENT),
                );
            });
            ui.add_space(theme::SPACING_XS);
            widgets::step_indicator(ui, self.step);
            ui.add_space(theme::SPACING_XS);
        });

        // ── Footer with navigation ─────────────────────────────────
        egui::TopBottomPanel::bottom("navigation").show(ctx, |ui| {
            ui.add_space(theme::SPACING_SM);
            ui.horizontal(|ui| {
                ui.add_space(theme::SPACING_MD);

                if self.step != WizardStep::Installing {
                    if let Some(prev) = self.step.prev() {
                        if ui.button("\u{2190} Back").clicked() {
                            self.step = prev;
                        }
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(theme::SPACING_MD);

                    match self.step {
                        WizardStep::Configure => {
                            if widgets::primary_button_enabled(
                                ui,
                                self.config_valid,
                                "Next \u{2192}",
                            )
                            .clicked()
                            {
                                if let Some(next) = self.step.next() {
                                    self.step = next;
                                }
                            }
                        }
                        WizardStep::Summary => {
                            if widgets::primary_button_enabled(
                                ui,
                                self.install.confirmed,
                                "Install \u{2192}",
                            )
                            .clicked()
                            {
                                if let Some(next) = self.step.next() {
                                    self.step = next;
                                    self.start_installation();
                                }
                            }
                        }
                        WizardStep::Installing => {
                            if self.install.finished
                                && widgets::primary_button(ui, "Close").clicked()
                            {
                                std::process::exit(0);
                            }
                        }
                    }
                });
            });
            ui.add_space(theme::SPACING_SM);
        });

        // ── Main content ───────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(theme::SPACING_MD);

            match self.step {
                WizardStep::Configure => {
                    self.config_valid = panels::configure::show(
                        ui,
                        &mut self.disk,
                        &mut self.system,
                        &mut self.user,
                        &mut self.packages,
                    );
                }
                WizardStep::Summary => {
                    panels::summary::show(
                        ui,
                        &self.disk,
                        &self.system,
                        &self.user,
                        &self.packages,
                        &mut self.install,
                    );

                    if self.install.save_requested {
                        self.install.save_requested = false;
                        self.save_config();
                    }
                    if self.install.preflight_requested {
                        self.install.preflight_requested = false;
                        self.start_preflight();
                    }
                    if self.install.rehearsal_requested {
                        self.install.rehearsal_requested = false;
                        self.start_rehearsal();
                    }
                }
                WizardStep::Installing => {
                    panels::progress::show(ui, &self.install);
                }
            }
        });
    }
}
