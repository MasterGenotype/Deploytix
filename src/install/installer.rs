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
    /// LUKS container for root (for encrypted installations)
    luks_container: Option<LuksContainer>,
    /// LUKS1 container for /boot (when boot_encryption is enabled)
    luks_boot_container: Option<LuksContainer>,
}

impl Installer {
    pub fn new(config: DeploymentConfig, dry_run: bool) -> Self {
        Self {
            config,
            cmd: CommandRunner::new(dry_run),
            layout: None,
            luks_container: None,
            luks_boot_container: None,
        }
    }

    /// Run the full installation process
    pub fn run(mut self) -> Result<()> {
        info!(
            "Starting Deploytix installation on {} ({} layout, {} init)",
            self.config.disk.device, self.config.disk.layout, self.config.system.init
        );

        // Phase 1: Preparation
        self.prepare()?;

        // Phase 2: Disk operations
        self.partition_disk()?;

        // Phase 2.5: LUKS setup (for CryptoSubvolume layout)
        // Follows canonical BTRFS subvolume setup order:
        //   1. Partition and format all partitions
        //   2. Mount each partition to its filesystem mountpoint
        //   3. Create subvolumes inside the mountpoint (@ prefixed)
        //   4. Unmount and remount with proper subvol= options
        if self.config.disk.layout == PartitionLayout::CryptoSubvolume {
            self.setup_encryption()?;
            self.format_crypto_partitions()?;
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

        info!("Installation to {} finished successfully", self.config.disk.device);
        println!("\n✓ Installation completed successfully!");
        println!("  You can now reboot into your new Artix Linux system.");

        Ok(())
    }

    /// Prepare for installation
    fn prepare(&mut self) -> Result<()> {
        info!("[Phase 1/6] Preparing installation for {}", self.config.disk.device);

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
        info!("[Phase 2/6] Partitioning {} with {} layout", self.config.disk.device, self.config.disk.layout);

        let layout = self.layout.as_ref().unwrap();
        apply_partitions(&self.cmd, &self.config.disk.device, layout)?;

        Ok(())
    }

    /// Format partitions
    fn format_partitions(&self) -> Result<()> {
        info!("[Phase 2/6] Formatting partitions on {} as {}", self.config.disk.device, self.config.disk.filesystem);

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
        info!("[Phase 2/6] Mounting partitions to {}", INSTALL_ROOT);

        let layout = self.layout.as_ref().unwrap();
        mount_partitions(&self.cmd, &self.config.disk.device, layout, INSTALL_ROOT)?;

        Ok(())
    }

    /// Install base system using basestrap
    fn install_base_system(&self) -> Result<()> {
        info!("[Phase 3/6] Installing base system via basestrap");

        run_basestrap(&self.cmd, &self.config, INSTALL_ROOT)?;

        Ok(())
    }

    /// Generate fstab
    fn generate_fstab(&self) -> Result<()> {
        info!("[Phase 3/6] Generating /etc/fstab with partition UUIDs");

        let layout = self.layout.as_ref().unwrap();
        generate_fstab(&self.cmd, &self.config.disk.device, layout, INSTALL_ROOT)?;

        Ok(())
    }

    /// Configure the system in chroot
    fn configure_system(&self) -> Result<()> {
        info!("[Phase 4/6] Configuring system in chroot (locale, users, bootloader, network, services)");

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
                info!("[Phase 5/6] Skipping desktop environment (none selected)");
            }
            DesktopEnvironment::Kde => {
                info!("[Phase 5/6] Installing KDE Plasma desktop environment");
                desktop::kde::install(&self.cmd, &self.config, INSTALL_ROOT)?;
            }
            DesktopEnvironment::Gnome => {
                info!("[Phase 5/6] Installing GNOME desktop environment");
                desktop::gnome::install(&self.cmd, &self.config, INSTALL_ROOT)?;
            }
            DesktopEnvironment::Xfce => {
                info!("[Phase 5/6] Installing XFCE desktop environment");
                desktop::xfce::install(&self.cmd, &self.config, INSTALL_ROOT)?;
            }
        }

        Ok(())
    }

    /// Finalize installation
    fn finalize(&self) -> Result<()> {
        info!("[Phase 6/6] Finalizing installation (regenerating initramfs, unmounting)");

        // Regenerate initramfs
        self.cmd.run_in_chroot(INSTALL_ROOT, "mkinitcpio -P")?;

        // Unmount all partitions
        unmount_all(&self.cmd, INSTALL_ROOT)?;

        // Close LUKS boot container if opened (close before root)
        if let Some(ref boot_container) = self.luks_boot_container {
            configure::encryption::close_luks(&self.cmd, &boot_container.mapper_name)?;
        }

        // Close LUKS root container if opened
        if let Some(ref container) = self.luks_container {
            configure::encryption::close_luks(&self.cmd, &container.mapper_name)?;
        }

        Ok(())
    }

    // ==================== ENCRYPTION-SPECIFIC METHODS ====================

    /// Setup LUKS encryption
    fn setup_encryption(&mut self) -> Result<()> {
        info!("[Phase 2/6] Setting up LUKS2 encryption on {}", self.config.disk.device);

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

        // Setup LUKS1 encryption on /boot partition if enabled
        if self.config.disk.boot_encryption {
            let boot_part = layout
                .partitions
                .iter()
                .find(|p| p.is_boot_fs)
                .ok_or_else(|| DeploytixError::ConfigError("No Boot partition found in layout".to_string()))?;

            let boot_container = configure::encryption::setup_boot_encryption(
                &self.cmd,
                &self.config,
                &self.config.disk.device,
                boot_part.number,
            )?;

            self.luks_boot_container = Some(boot_container);
        }

        Ok(())
    }

    /// Format all partitions for CryptoSubvolume layout upfront
    ///
    /// Step 1: Partition and format all partitions before any mounting:
    ///   - BTRFS filesystem on the LUKS-mapped device
    ///   - BOOT partition as BTRFS (on LUKS1 mapped device if boot_encryption enabled)
    ///   - EFI partition as FAT32
    fn format_crypto_partitions(&self) -> Result<()> {
        info!("[Phase 2/6] Formatting encrypted partitions (btrfs on LUKS, boot, EFI)");

        let container = self.luks_container.as_ref().unwrap();
        let layout = self.layout.as_ref().unwrap();

        // Format BTRFS filesystem on LUKS-mapped device
        create_btrfs_filesystem(&self.cmd, &container.mapped_path, "ROOT")?;

        // Format BOOT partition as BTRFS
        // If boot_encryption is enabled, format the LUKS1-mapped device instead of raw partition
        if let Some(ref boot_container) = self.luks_boot_container {
            format_boot(&self.cmd, &boot_container.mapped_path)?;
        } else {
            let boot_part = layout
                .partitions
                .iter()
                .find(|p| p.is_boot_fs)
                .ok_or_else(|| DeploytixError::ConfigError("No Boot Partition Found in Layout".to_string()))?;
            let boot_device = partition_path(&self.config.disk.device, boot_part.number);
            format_boot(&self.cmd, &boot_device)?;
        }

        // Format EFI partition as FAT32
        let efi_part = layout
            .partitions
            .iter()
            .find(|p| p.is_efi)
            .ok_or_else(|| DeploytixError::ConfigError("No EFI partition found in layout".to_string()))?;
        let efi_device = partition_path(&self.config.disk.device, efi_part.number);
        format_efi(&self.cmd, &efi_device)?;

        info!("Encrypted partitions formatted successfully");
        Ok(())
    }

    /// Create btrfs subvolumes inside LUKS container
    ///
    /// Step 2-3: Mount raw BTRFS filesystem to INSTALL_ROOT,
    /// create subvolumes inside it (@ prefixed), then unmount.
    fn create_btrfs_subvolumes(&self) -> Result<()> {
        info!("[Phase 2/6] Creating btrfs subvolumes inside LUKS container");

        let container = self.luks_container.as_ref().unwrap();
        let layout = self.layout.as_ref().unwrap();

        // Mount raw btrfs to INSTALL_ROOT, create subvolumes, unmount
        if let Some(ref subvolumes) = layout.subvolumes {
            create_btrfs_subvolumes(
                &self.cmd,
                &container.mapped_path,
                subvolumes,
                INSTALL_ROOT,
            )?;
        }

        Ok(())
    }

    /// Mount btrfs subvolumes and other partitions for installation
    ///
    /// Step 4: Remount with proper subvol= options to the parent
    /// directory mountpoints, then mount BOOT and EFI.
    fn mount_crypto_subvolumes(&self) -> Result<()> {
        info!("[Phase 2/6] Mounting btrfs subvolumes with subvol= options to {}", INSTALL_ROOT);

        let container = self.luks_container.as_ref().unwrap();
        let layout = self.layout.as_ref().unwrap();

        // Remount each subvolume with proper subvol= option
        if let Some(ref subvolumes) = layout.subvolumes {
            mount_btrfs_subvolumes(
                &self.cmd,
                &container.mapped_path,
                subvolumes,
                INSTALL_ROOT,
            )?;
        }

        // Mount BOOT partition (from LUKS1 mapped device if boot_encryption is enabled)
        let boot_source = if let Some(ref boot_container) = self.luks_boot_container {
            boot_container.mapped_path.clone()
        } else {
            let boot_part = layout
                .partitions
                .iter()
                .find(|p| p.is_boot_fs)
                .ok_or_else(|| DeploytixError::ConfigError("No Boot Partition Found in Layout".to_string()))?;
            partition_path(&self.config.disk.device, boot_part.number)
        };
        let boot_mount = format!("{}/boot", INSTALL_ROOT);

        if !self.cmd.is_dry_run() {
            std::fs::create_dir_all(&boot_mount)?;
        }
        self.cmd.run("mount", &[&boot_source, &boot_mount])?;

        // Mount EFI partition
        let efi_part = layout
            .partitions
            .iter()
            .find(|p| p.is_efi)
            .ok_or_else(|| DeploytixError::ConfigError("No EFI partition found in layout".to_string()))?;
        let efi_device = partition_path(&self.config.disk.device, efi_part.number);
        let efi_mount = format!("{}/boot/efi", INSTALL_ROOT);

        if !self.cmd.is_dry_run() {
            std::fs::create_dir_all(&efi_mount)?;
        }
        self.cmd.run("mount", &[&efi_device, &efi_mount])?;

        Ok(())
    }

    /// Generate fstab for encrypted btrfs subvolumes
    fn generate_fstab_crypto(&self) -> Result<()> {
        info!("[Phase 3/6] Generating /etc/fstab for encrypted btrfs subvolumes");

        let container = self.luks_container.as_ref().unwrap();
        let layout = self.layout.as_ref().unwrap();

        // Use LUKS1 mapped device for boot if boot_encryption is enabled,
        // otherwise use the raw partition
        let boot_device = if let Some(ref boot_container) = self.luks_boot_container {
            boot_container.mapped_path.clone()
        } else {
            let boot_part = layout
                .partitions
                .iter()
                .find(|p| p.is_boot_fs)
                .ok_or_else(|| DeploytixError::ConfigError("No Boot partition found in layout".to_string()))?;
            partition_path(&self.config.disk.device, boot_part.number)
        };

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
                &boot_device,
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

        // Find boot partition number for boot encryption entry
        let boot_luks_partition = if self.config.disk.boot_encryption {
            let boot_part = layout
                .partitions
                .iter()
                .find(|p| p.is_boot_fs)
                .map(|p| p.number);
            boot_part
        } else {
            None
        };

        generate_crypttab(
            &self.cmd,
            &self.config,
            &self.config.disk.device,
            luks_part.number,
            boot_luks_partition,
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
