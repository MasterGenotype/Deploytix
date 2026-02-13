//! Main GUI application

use crate::config::{
    Bootloader, DeploymentConfig, DesktopConfig, DesktopEnvironment, DiskConfig, Filesystem,
    InitSystem, NetworkBackend, NetworkConfig, PartitionLayout, SystemConfig, UserConfig,
};
use crate::disk::detection::{list_block_devices, BlockDevice};
use crate::install::Installer;
use eframe::egui;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use super::panels;

/// Wizard steps
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardStep {
    DiskSelection,
    DiskConfig,
    SystemConfig,
    UserConfig,
    NetworkDesktop,
    Summary,
    Installing,
}

impl WizardStep {
    fn next(&self) -> Option<Self> {
        match self {
            Self::DiskSelection => Some(Self::DiskConfig),
            Self::DiskConfig => Some(Self::SystemConfig),
            Self::SystemConfig => Some(Self::UserConfig),
            Self::UserConfig => Some(Self::NetworkDesktop),
            Self::NetworkDesktop => Some(Self::Summary),
            Self::Summary => Some(Self::Installing),
            Self::Installing => None,
        }
    }

    fn prev(&self) -> Option<Self> {
        match self {
            Self::DiskSelection => None,
            Self::DiskConfig => Some(Self::DiskSelection),
            Self::SystemConfig => Some(Self::DiskConfig),
            Self::UserConfig => Some(Self::SystemConfig),
            Self::NetworkDesktop => Some(Self::UserConfig),
            Self::Summary => Some(Self::NetworkDesktop),
            Self::Installing => None, // Can't go back during installation
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::DiskSelection => "Disk",
            Self::DiskConfig => "Partitions",
            Self::SystemConfig => "System",
            Self::UserConfig => "User",
            Self::NetworkDesktop => "Network",
            Self::Summary => "Review",
            Self::Installing => "Install",
        }
    }
}

/// Installation progress message
pub enum InstallMessage {
    Status(String),
    Progress(f32),
    Log(String),
    Finished,
    Error(String),
}

/// Main GUI application state
pub struct DeploytixGui {
    // Current wizard step
    step: WizardStep,

    // Disk selection
    devices: Vec<BlockDevice>,
    selected_device_index: Option<usize>,
    refreshing_disks: bool,

    // Disk config
    partition_layout: PartitionLayout,
    filesystem: Filesystem,
    encryption: bool,
    encryption_password: String,
    boot_encryption: bool,

    // System config
    init_system: InitSystem,
    bootloader: Bootloader,
    timezone: String,
    locale: String,
    keymap: String,
    hostname: String,

    // User config
    username: String,
    user_password: String,
    user_password_confirm: String,
    sudoer: bool,

    // Network & Desktop
    network_backend: NetworkBackend,
    desktop_env: DesktopEnvironment,

    // Summary
    dry_run: bool,
    confirmed: bool,

    // Installation progress
    install_status: String,
    install_progress: f32,
    install_logs: Vec<String>,
    install_finished: bool,
    install_error: Option<String>,
    install_receiver: Option<Receiver<InstallMessage>>,
}

impl Default for DeploytixGui {
    fn default() -> Self {
        Self {
            step: WizardStep::DiskSelection,
            devices: Vec::new(),
            selected_device_index: None,
            refreshing_disks: true, // Start by refreshing

            partition_layout: PartitionLayout::Standard,
            filesystem: Filesystem::Btrfs,
            encryption: false,
            encryption_password: String::new(),
            boot_encryption: false,

            init_system: InitSystem::Runit,
            bootloader: Bootloader::Grub,
            timezone: "UTC".to_string(),
            locale: "en_US.UTF-8".to_string(),
            keymap: "us".to_string(),
            hostname: "artix".to_string(),

            username: String::new(),
            user_password: String::new(),
            user_password_confirm: String::new(),
            sudoer: true,

            network_backend: NetworkBackend::Iwd,
            desktop_env: DesktopEnvironment::None,

            dry_run: false,
            confirmed: false,

            install_status: String::new(),
            install_progress: 0.0,
            install_logs: Vec::new(),
            install_finished: false,
            install_error: None,
            install_receiver: None,
        }
    }
}

impl DeploytixGui {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self::default()
    }

    fn refresh_disks(&mut self) {
        match list_block_devices(false) {
            Ok(devices) => {
                self.devices = devices;
                self.selected_device_index = None;
            }
            Err(e) => {
                self.devices = Vec::new();
                self.install_logs.push(format!("Error listing disks: {}", e));
            }
        }
        self.refreshing_disks = false;
    }

    fn build_config(&self) -> DeploymentConfig {
        let device_path = self
            .selected_device_index
            .and_then(|i| self.devices.get(i))
            .map(|d| d.path.clone())
            .unwrap_or_default();

        DeploymentConfig {
            disk: DiskConfig {
                device: device_path,
                layout: self.partition_layout.clone(),
                filesystem: self.filesystem.clone(),
                encryption: self.encryption,
                encryption_password: if self.encryption {
                    Some(self.encryption_password.clone())
                } else {
                    None
                },
                luks_mapper_name: "Crypt-Root".to_string(),
                boot_encryption: self.boot_encryption,
                luks_boot_mapper_name: "Crypt-Boot".to_string(),
                keyfile_path: None,
                keyfile_enabled: self.partition_layout == PartitionLayout::CryptoSubvolume,
            },
            system: SystemConfig {
                init: self.init_system.clone(),
                bootloader: self.bootloader.clone(),
                timezone: self.timezone.clone(),
                locale: self.locale.clone(),
                keymap: self.keymap.clone(),
                hostname: self.hostname.clone(),
                hibernation: false,
            },
            user: UserConfig {
                name: self.username.clone(),
                password: self.user_password.clone(),
                groups: vec![
                    "wheel".to_string(),
                    "video".to_string(),
                    "audio".to_string(),
                    "network".to_string(),
                    "log".to_string(),
                ],
                sudoer: self.sudoer,
            },
            network: NetworkConfig {
                backend: self.network_backend.clone(),
            },
            desktop: DesktopConfig {
                environment: self.desktop_env.clone(),
                display_manager: None,
            },
        }
    }

    fn start_installation(&mut self) {
        let config = self.build_config();
        let dry_run = self.dry_run;

        let (tx, rx): (Sender<InstallMessage>, Receiver<InstallMessage>) = channel();
        self.install_receiver = Some(rx);

        self.install_status = "Starting installation...".to_string();
        self.install_progress = 0.0;
        self.install_logs.clear();

        // Spawn installation thread
        thread::spawn(move || {
            let _ = tx.send(InstallMessage::Log(format!(
                "Starting {} installation on {}",
                if dry_run { "dry-run" } else { "real" },
                config.disk.device
            )));
            let _ = tx.send(InstallMessage::Progress(0.05));

            // Validate config first
            if let Err(e) = config.validate() {
                let _ = tx.send(InstallMessage::Error(format!("Configuration error: {}", e)));
                return;
            }

            let _ = tx.send(InstallMessage::Log("Configuration validated".to_string()));
            let _ = tx.send(InstallMessage::Progress(0.1));

            // Check root (skip in dry-run)
            if !dry_run && !nix::unistd::geteuid().is_root() {
                let _ = tx.send(InstallMessage::Error(
                    "Must run as root for real installation".to_string(),
                ));
                return;
            }

            let _ = tx.send(InstallMessage::Status("Running installer...".to_string()));
            let _ = tx.send(InstallMessage::Progress(0.15));

            // Run installer
            let installer = Installer::new(config, dry_run).with_skip_confirm(true);
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
        let mut should_clear_receiver = false;

        if let Some(ref rx) = self.install_receiver {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    InstallMessage::Status(s) => self.install_status = s,
                    InstallMessage::Progress(p) => self.install_progress = p,
                    InstallMessage::Log(s) => self.install_logs.push(s),
                    InstallMessage::Finished => {
                        self.install_finished = true;
                        should_clear_receiver = true;
                    }
                    InstallMessage::Error(e) => {
                        self.install_error = Some(e);
                        self.install_finished = true;
                        should_clear_receiver = true;
                    }
                }
            }
        }

        if should_clear_receiver {
            self.install_receiver = None;
        }
    }
}

impl eframe::App for DeploytixGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Handle disk refresh
        if self.refreshing_disks {
            self.refresh_disks();
        }

        // Poll installation messages
        if self.install_receiver.is_some() {
            self.poll_install_messages();
            ctx.request_repaint(); // Keep updating during installation
        }

        // Top panel with step indicators
        egui::TopBottomPanel::top("wizard_steps").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.heading("🚀 Deploytix");
                ui.add_space(32.0);

                let steps = [
                    WizardStep::DiskSelection,
                    WizardStep::DiskConfig,
                    WizardStep::SystemConfig,
                    WizardStep::UserConfig,
                    WizardStep::NetworkDesktop,
                    WizardStep::Summary,
                    WizardStep::Installing,
                ];

                for (i, s) in steps.iter().enumerate() {
                    let is_current = self.step == *s;
                    let is_past = (*s as usize) < (self.step as usize);

                    let text = if is_current {
                        egui::RichText::new(s.label()).strong().color(egui::Color32::WHITE)
                    } else if is_past {
                        egui::RichText::new(s.label()).color(egui::Color32::GREEN)
                    } else {
                        egui::RichText::new(s.label()).color(egui::Color32::GRAY)
                    };

                    ui.label(text);

                    if i < steps.len() - 1 {
                        ui.label("→");
                    }
                }
            });
            ui.add_space(8.0);
        });

        // Bottom panel with navigation buttons
        let mut can_proceed = false;
        egui::TopBottomPanel::bottom("navigation").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);

                // Back button
                if self.step != WizardStep::Installing {
                    if let Some(prev) = self.step.prev() {
                        if ui.button("← Back").clicked() {
                            self.step = prev;
                        }
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(16.0);

                    match self.step {
                        WizardStep::Installing => {
                            if self.install_finished {
                                if ui.button("Close").clicked() {
                                    std::process::exit(0);
                                }
                            }
                        }
                        WizardStep::Summary => {
                            // Enable when user has confirmed the warning OR is running dry-run
                            let enabled = self.confirmed || self.dry_run;
                            if ui.add_enabled(enabled, egui::Button::new("Install →")).clicked() {
                                if let Some(next) = self.step.next() {
                                    self.step = next;
                                    self.start_installation();
                                }
                            }
                        }
                        _ => {
                            // Will be set by panel
                        }
                    }
                });
            });
            ui.add_space(8.0);
        });

        // Main content panel
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(16.0);

            let panel_can_proceed = match self.step {
                WizardStep::DiskSelection => {
                    panels::disk_selection_panel(
                        ui,
                        &self.devices,
                        &mut self.selected_device_index,
                        &mut self.refreshing_disks,
                    )
                }
                WizardStep::DiskConfig => {
                    panels::disk_config_panel(
                        ui,
                        &mut self.partition_layout,
                        &mut self.filesystem,
                        &mut self.encryption,
                        &mut self.encryption_password,
                        &mut self.boot_encryption,
                    )
                }
                WizardStep::SystemConfig => {
                    panels::system_config_panel(
                        ui,
                        &mut self.init_system,
                        &mut self.bootloader,
                        &mut self.timezone,
                        &mut self.locale,
                        &mut self.keymap,
                        &mut self.hostname,
                    )
                }
                WizardStep::UserConfig => {
                    panels::user_config_panel(
                        ui,
                        &mut self.username,
                        &mut self.user_password,
                        &mut self.user_password_confirm,
                        &mut self.sudoer,
                    )
                }
                WizardStep::NetworkDesktop => {
                    panels::network_desktop_panel(
                        ui,
                        &mut self.network_backend,
                        &mut self.desktop_env,
                    )
                }
                WizardStep::Summary => {
                    let device_path = self
                        .selected_device_index
                        .and_then(|i| self.devices.get(i))
                        .map(|d| d.path.as_str())
                        .unwrap_or("(none)");

                    panels::summary_panel(
                        ui,
                        device_path,
                        &self.partition_layout,
                        &self.filesystem,
                        self.encryption,
                        &self.init_system,
                        &self.bootloader,
                        &self.hostname,
                        &self.username,
                        &self.network_backend,
                        &self.desktop_env,
                        &mut self.dry_run,
                        &mut self.confirmed,
                    )
                }
                WizardStep::Installing => {
                    panels::progress_panel(
                        ui,
                        &self.install_status,
                        self.install_progress,
                        &self.install_logs,
                        self.install_finished,
                        self.install_error.as_deref(),
                    );
                    false
                }
            };

            can_proceed = panel_can_proceed;

            // Show Next button for non-summary, non-installing steps
            if !matches!(self.step, WizardStep::Summary | WizardStep::Installing) {
                ui.add_space(16.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if ui.add_enabled(can_proceed, egui::Button::new("Next →")).clicked() {
                        if let Some(next) = self.step.next() {
                            self.step = next;
                        }
                    }
                });
            }
        });
    }
}
