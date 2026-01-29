//! Main installation orchestrator

use crate::config::DeploymentConfig;
use crate::configure;
use crate::desktop;
use crate::disk::detection::get_device_info;
use crate::disk::formatting::format_all_partitions;
use crate::disk::layouts::{compute_layout, print_layout_summary, ComputedLayout};
use crate::disk::partitioning::apply_partitions;
use crate::install::{generate_fstab, mount_partitions, run_basestrap, unmount_all};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use crate::utils::prompt::warn_confirm;
use tracing::info;

/// Installation target path
pub const INSTALL_ROOT: &str = "/install";

/// Main installer struct
pub struct Installer {
    config: DeploymentConfig,
    cmd: CommandRunner,
    layout: Option<ComputedLayout>,
}

impl Installer {
    pub fn new(config: DeploymentConfig, dry_run: bool) -> Self {
        Self {
            config,
            cmd: CommandRunner::new(dry_run),
            layout: None,
        }
    }

    /// Run the full installation process
    pub fn run(mut self) -> Result<()> {
        info!("Starting Deploytix installation");

        // Phase 1: Preparation
        self.prepare()?;

        // Phase 2: Disk operations
        self.partition_disk()?;
        self.format_partitions()?;
        self.mount_partitions()?;

        // Phase 3: Base system
        self.install_base_system()?;
        self.generate_fstab()?;

        // Phase 4: System configuration
        self.configure_system()?;

        // Phase 5: Desktop environment (if selected)
        self.install_desktop()?;

        // Phase 6: Finalization
        self.finalize()?;

        info!("Installation complete!");
        println!("\n✓ Installation completed successfully!");
        println!("  You can now reboot into your new Artix Linux system.");

        Ok(())
    }

    /// Prepare for installation
    fn prepare(&mut self) -> Result<()> {
        info!("Preparing installation");

        // Get device info and compute layout
        let device_info = get_device_info(&self.config.disk.device)?;
        let disk_mib = device_info.size_mib();

        info!(
            "Target disk: {} ({}, {} MiB)",
            self.config.disk.device,
            device_info.model.as_deref().unwrap_or("Unknown"),
            disk_mib
        );

        // Compute partition layout
        let layout = compute_layout(&self.config.disk.layout, disk_mib)?;
        print_layout_summary(&layout);
        self.layout = Some(layout);

        // Confirm with user
        let warning = format!(
            "This will ERASE ALL DATA on {}. This operation cannot be undone!",
            self.config.disk.device
        );

        if !self.cmd.is_dry_run() && !warn_confirm(&warning)? {
            return Err(crate::utils::error::DeploytixError::UserCancelled);
        }

        // Create installation directory
        if !self.cmd.is_dry_run() {
            std::fs::create_dir_all(INSTALL_ROOT)?;
        }

        Ok(())
    }

    /// Partition the disk
    fn partition_disk(&self) -> Result<()> {
        info!("Partitioning disk");

        let layout = self.layout.as_ref().unwrap();
        apply_partitions(&self.cmd, &self.config.disk.device, layout)?;

        Ok(())
    }

    /// Format partitions
    fn format_partitions(&self) -> Result<()> {
        info!("Formatting partitions");

        let layout = self.layout.as_ref().unwrap();
        format_all_partitions(
            &self.cmd,
            &self.config.disk.device,
            layout,
            &self.config.disk.filesystem,
        )?;

        Ok(())
    }

    /// Mount partitions
    fn mount_partitions(&self) -> Result<()> {
        info!("Mounting partitions");

        let layout = self.layout.as_ref().unwrap();
        mount_partitions(&self.cmd, &self.config.disk.device, layout, INSTALL_ROOT)?;

        Ok(())
    }

    /// Install base system using basestrap
    fn install_base_system(&self) -> Result<()> {
        info!("Installing base system");

        run_basestrap(&self.cmd, &self.config, INSTALL_ROOT)?;

        Ok(())
    }

    /// Generate fstab
    fn generate_fstab(&self) -> Result<()> {
        info!("Generating fstab");

        let layout = self.layout.as_ref().unwrap();
        generate_fstab(&self.cmd, &self.config.disk.device, layout, INSTALL_ROOT)?;

        Ok(())
    }

    /// Configure the system in chroot
    fn configure_system(&self) -> Result<()> {
        info!("Configuring system");

        // Locale and timezone
        configure::locale::configure_locale(&self.cmd, &self.config, INSTALL_ROOT)?;

        // User creation
        configure::users::create_user(&self.cmd, &self.config, INSTALL_ROOT)?;

        // mkinitcpio
        configure::mkinitcpio::configure_mkinitcpio(&self.cmd, &self.config, INSTALL_ROOT)?;

        // Bootloader
        configure::bootloader::install_bootloader(
            &self.cmd,
            &self.config,
            &self.config.disk.device,
            INSTALL_ROOT,
        )?;

        // Network
        configure::network::configure_network(&self.cmd, &self.config, INSTALL_ROOT)?;

        // Services
        configure::services::enable_services(&self.cmd, &self.config, INSTALL_ROOT)?;

        Ok(())
    }

    /// Install desktop environment
    fn install_desktop(&self) -> Result<()> {
        use crate::config::DesktopEnvironment;

        match &self.config.desktop.environment {
            DesktopEnvironment::None => {
                info!("No desktop environment selected");
            }
            DesktopEnvironment::Kde => {
                info!("Installing KDE Plasma");
                desktop::kde::install(&self.cmd, &self.config, INSTALL_ROOT)?;
            }
            DesktopEnvironment::Gnome => {
                info!("Installing GNOME");
                desktop::gnome::install(&self.cmd, &self.config, INSTALL_ROOT)?;
            }
            DesktopEnvironment::Xfce => {
                info!("Installing XFCE");
                desktop::xfce::install(&self.cmd, &self.config, INSTALL_ROOT)?;
            }
        }

        Ok(())
    }

    /// Finalize installation
    fn finalize(&self) -> Result<()> {
        info!("Finalizing installation");

        // Regenerate initramfs
        self.cmd.run_in_chroot(INSTALL_ROOT, "mkinitcpio -P")?;

        // Unmount all partitions
        unmount_all(&self.cmd, INSTALL_ROOT)?;

        Ok(())
    }
}
