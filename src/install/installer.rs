//! Main installation orchestrator

use crate::config::{DeploymentConfig, PartitionLayout};
use crate::configure;
use crate::configure::encryption::LuksContainer;
use crate::desktop;
use crate::disk::detection::{get_device_info, partition_path};
use crate::disk::formatting::{
    format_all_partitions, format_efi, format_boot, create_btrfs_filesystem, create_btrfs_subvolumes, mount_btrfs_subvolumes
};
use crate::disk::layouts::{compute_layout, print_layout_summary, ComputedLayout};
use crate::disk::partitioning::apply_partitions;
use crate::install::{
    generate_fstab, generate_fstab_crypto_subvolume, mount_partitions, run_basestrap, unmount_all
};
use crate::install::crypttab::generate_crypttab;
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use crate::utils::prompt::warn_confirm;
use tracing::info;

/// Installation target path
pub const INSTALL_ROOT: &str = "/install";

/// Main installer struct
pub struct Installer {
    config: DeploymentConfig,
    cmd: CommandRunner,
    layout: Option<ComputedLayout>,
    /// LUKS container (for encrypted installations)
    luks_container: Option<LuksContainer>,
}

impl Installer {
    pub fn new(config: DeploymentConfig, dry_run: bool) -> Self {
        Self {
            config,
            cmd: CommandRunner::new(dry_run),
            layout: None,
            luks_container: None,
        }
    }

    /// Run the full installation process
    pub fn run(mut self) -> Result<()> {
        info!("Starting Deploytix installation");

        // Phase 1: Preparation
        self.prepare()?;

        // Phase 2: Disk operations
        self.partition_disk()?;

        // Phase 2.5: LUKS setup (for CryptoSubvolume layout)
        if self.config.disk.layout == PartitionLayout::CryptoSubvolume {
            self.setup_encryption()?;
            self.create_btrfs_subvolumes()?;
            self.mount_crypto_subvolumes()?;
        } else {
            self.format_partitions()?;
            self.mount_partitions()?;
        }

        // Phase 3: Base system
        self.install_base_system()?;

        // Phase 3.5: Generate fstab (different method for encrypted)
        if self.config.disk.layout == PartitionLayout::CryptoSubvolume {
            self.generate_fstab_crypto()?;
        } else {
            self.generate_fstab()?;
        }

        // Phase 3.6: Crypttab (for encrypted systems)
        if self.config.disk.encryption && self.config.disk.layout == PartitionLayout::CryptoSubvolume {
            self.generate_crypttab()?;
        }

        // Phase 4: System configuration
        self.configure_system()?;

        // Phase 4.5: Custom hooks (for encrypted systems)
        if self.config.disk.encryption && self.config.disk.layout == PartitionLayout::CryptoSubvolume {
            self.install_custom_hooks()?;
        }

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

        // Bootloader (use layout-aware version for encrypted systems)
        if self.config.disk.layout == PartitionLayout::CryptoSubvolume {
            let layout = self.layout.as_ref().unwrap();
            configure::bootloader::install_bootloader_with_layout(
                &self.cmd,
                &self.config,
                &self.config.disk.device,
                layout,
                INSTALL_ROOT,
            )?;
        } else {
            configure::bootloader::install_bootloader(
                &self.cmd,
                &self.config,
                &self.config.disk.device,
                INSTALL_ROOT,
            )?;
        }

        // Network
        configure::network::configure_network(&self.cmd, &self.config, INSTALL_ROOT)?;

        // greetd configuration (if desktop environment selected)
        configure::greetd::configure_greetd(&self.cmd, &self.config, INSTALL_ROOT)?;

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

        // Close LUKS container if opened
        if let Some(ref container) = self.luks_container {
            configure::encryption::close_luks(&self.cmd, &container.mapper_name)?;
        }

        Ok(())
    }

    // ==================== ENCRYPTION-SPECIFIC METHODS ====================

    /// Setup LUKS encryption
    fn setup_encryption(&mut self) -> Result<()> {
        info!("Setting up LUKS encryption");

        let layout = self.layout.as_ref().unwrap();
        let luks_part = layout
            .partitions
            .iter()
            .find(|p| p.is_luks)
            .ok_or_else(|| DeploytixError::ConfigError("No LUKS partition found in layout".to_string()))?;

        let container = configure::encryption::setup_encryption(
            &self.cmd,
            &self.config,
            &self.config.disk.device,
            luks_part.number,
        )?;

        self.luks_container = Some(container);
        Ok(())
    }

    /// Create btrfs subvolumes inside LUKS container
    fn create_btrfs_subvolumes(&self) -> Result<()> {
        info!("Creating btrfs filesystem and subvolumes");

        let container = self.luks_container.as_ref().unwrap();
        let layout = self.layout.as_ref().unwrap();

        // Create btrfs filesystem
        create_btrfs_filesystem(&self.cmd, &container.mapped_path, "ROOT")?;

        // Create subvolumes
        if let Some(ref subvolumes) = layout.subvolumes {
            create_btrfs_subvolumes(
                &self.cmd,
                &container.mapped_path,
                subvolumes,
                "/tmp/deploytix-btrfs",
            )?;
        }

        Ok(())
    }

    /// Mount btrfs subvolumes for installation
    fn mount_crypto_subvolumes(&self) -> Result<()> {
        info!("Mounting btrfs subvolumes");

        let container = self.luks_container.as_ref().unwrap();
        let layout = self.layout.as_ref().unwrap();

        if let Some(ref subvolumes) = layout.subvolumes {
            mount_btrfs_subvolumes(
                &self.cmd,
                &container.mapped_path,
                subvolumes,
                INSTALL_ROOT,
            )?;
        }
        // Format and mount BOOT partition
        let boot_part = layout
            .partitions
            .iter()
            .find(|p| p.is_boot_fs)
            .ok_or_else(|| DeploytixError::ConfigError("No Boot Partition Found in Layout".to_string()))?;
        
        let boot_device = partition_path(&self.config.disk.device, boot_part.number);
        let boot_mount = format!("{}/boot", INSTALL_ROOT);
        
        // Format Boot as BTRFS
        format_boot(&self.cmd, &boot_device)?;
        
        if !self.cmd.is_dry_run() {
            std::fs::create_dir_all(&boot_mount)?;
        }
        self.cmd.run("mount", &[&boot_device, &boot_mount])?;
        
        Ok::<(),
        // Format and mount EFI partition
        let efi_part = layout
            .partitions
            .iter()
            .find(|p| p.is_efi)
            .ok_or_else(|| DeploytixError::ConfigError("No EFI partition found in layout".to_string()))?;

        let efi_device = partition_path(&self.config.disk.device, efi_part.number);
        let efi_mount = format!("{}/boot/efi", INSTALL_ROOT);

        // Format EFI as FAT32 (this is skipped in format_all_partitions for CryptoSubvolume)
        format_efi(&self.cmd, &efi_device)?;

        if !self.cmd.is_dry_run() {
            std::fs::create_dir_all(&efi_mount)?;
        }
        self.cmd.run("mount", &[&efi_device, &efi_mount])?;

        Ok(())
    }

    /// Generate fstab for encrypted btrfs subvolumes
    fn generate_fstab_crypto(&self) -> Result<()> {
        info!("Generating fstab for encrypted system");

        let container = self.luks_container.as_ref().unwrap();
        let layout = self.layout.as_ref().unwrap();

        let efi_part = layout
            .partitions
            .iter()
            .find(|p| p.is_efi)
            .ok_or_else(|| DeploytixError::ConfigError("No EFI partition found in layout".to_string()))?;

        let efi_device = partition_path(&self.config.disk.device, efi_part.number);

        if let Some(ref subvolumes) = layout.subvolumes {
            generate_fstab_crypto_subvolume(
                &self.cmd,
                &container.mapped_path,
                &efi_device,
                subvolumes,
                INSTALL_ROOT,
            )?;
        }

        Ok(())
    }

    /// Generate crypttab
    fn generate_crypttab(&self) -> Result<()> {
        let layout = self.layout.as_ref().unwrap();
        let luks_part = layout
            .partitions
            .iter()
            .find(|p| p.is_luks)
            .ok_or_else(|| DeploytixError::ConfigError("No LUKS partition found in layout".to_string()))?;

        generate_crypttab(
            &self.cmd,
            &self.config,
            &self.config.disk.device,
            luks_part.number,
            INSTALL_ROOT,
        )
    }

    /// Install custom mkinitcpio hooks
    fn install_custom_hooks(&self) -> Result<()> {
        let layout = self.layout.as_ref().unwrap();

        configure::hooks::install_custom_hooks(
            &self.cmd,
            &self.config,
            layout,
            INSTALL_ROOT,
        )
    }
}
