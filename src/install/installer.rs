//! Main installation orchestrator

use crate::config::{DeploymentConfig, PartitionLayout};
use crate::configure;
use crate::configure::encryption::{LuksContainer, setup_multi_volume_encryption, close_multi_luks};
use crate::configure::keyfiles::{setup_keyfiles_for_volumes, VolumeKeyfile};
use crate::desktop;
use crate::disk::detection::{get_device_info, partition_path};
use crate::disk::formatting::{
    format_all_partitions, format_efi, format_boot, format_swap,
    create_btrfs_filesystem,
};
use crate::disk::layouts::{compute_layout, print_layout_summary, ComputedLayout, get_luks_partitions};
use crate::disk::partitioning::apply_partitions;
use crate::install::{
    generate_fstab, mount_partitions, run_basestrap, unmount_all
};
use crate::install::fstab::generate_fstab_multi_volume;
use crate::install::crypttab::generate_crypttab_multi_volume;
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use crate::utils::prompt::warn_confirm;
use std::fs;
use tracing::info;

/// Installation target path
pub const INSTALL_ROOT: &str = "/install";

/// Main installer struct
pub struct Installer {
    config: DeploymentConfig,
    cmd: CommandRunner,
    layout: Option<ComputedLayout>,
    /// LUKS containers for multi-volume encryption (root, usr, var, home)
    luks_containers: Vec<LuksContainer>,
    /// LUKS1 container for /boot (when boot_encryption is enabled)
    luks_boot_container: Option<LuksContainer>,
    /// Keyfiles for automatic unlocking
    keyfiles: Vec<VolumeKeyfile>,
    /// Skip interactive confirmation prompt (e.g. when GUI already confirmed)
    skip_confirm: bool,
}

impl Installer {
    pub fn new(config: DeploymentConfig, dry_run: bool) -> Self {
        Self {
            config,
            cmd: CommandRunner::new(dry_run),
            layout: None,
            luks_containers: Vec::new(),
            luks_boot_container: None,
            keyfiles: Vec::new(),
            skip_confirm: false,
        }
    }

    /// Skip the interactive confirmation prompt.
    /// Use this when confirmation has already been obtained (e.g. via GUI).
    pub fn with_skip_confirm(mut self, skip: bool) -> Self {
        self.skip_confirm = skip;
        self
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
        // Multi-volume encryption: separate LUKS containers for root, usr, var, home
        if self.config.disk.layout == PartitionLayout::CryptoSubvolume {
            self.setup_multi_volume_encryption()?;
            self.format_multi_volume_partitions()?;
            self.mount_multi_volume_partitions()?;
        } else {
            self.format_partitions()?;
            self.mount_partitions()?;
        }

        // Phase 3: Base system
        self.install_base_system()?;

        // Phase 3.5: Generate fstab (different method for encrypted)
        if self.config.disk.layout == PartitionLayout::CryptoSubvolume {
            self.generate_fstab_multi_volume()?;
        } else {
            self.generate_fstab()?;
        }

        // Phase 3.6: Crypttab and keyfiles (for multi-volume encrypted systems)
        if self.config.disk.encryption && self.config.disk.layout == PartitionLayout::CryptoSubvolume {
            self.setup_keyfiles()?;
            self.generate_crypttab_multi_volume()?;
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

        if !self.cmd.is_dry_run() && !self.skip_confirm && !warn_confirm(&warning)? {
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

        // Close LUKS boot container if opened (close before root volumes)
        if let Some(ref boot_container) = self.luks_boot_container {
            configure::encryption::close_luks(&self.cmd, &boot_container.mapper_name)?;
        }

        // Close all LUKS containers (in reverse order: home, var, usr, root)
        if !self.luks_containers.is_empty() {
            close_multi_luks(&self.cmd, &self.luks_containers)?;
        }

        Ok(())
    }

    // ==================== MULTI-VOLUME ENCRYPTION METHODS ====================

    /// Setup multi-volume LUKS encryption (root, usr, var, home)
    fn setup_multi_volume_encryption(&mut self) -> Result<()> {
        info!("[Phase 2/6] Setting up multi-volume LUKS2 encryption on {}", self.config.disk.device);

        let layout = self.layout.as_ref().unwrap();

        // Get all LUKS partitions from layout
        let luks_parts: Vec<(u32, &str)> = get_luks_partitions(layout)
            .iter()
            .map(|p| (p.number, p.name.as_str()))
            .collect();

        if luks_parts.is_empty() {
            return Err(DeploytixError::ConfigError(
                "No LUKS partitions found in layout".to_string(),
            ));
        }

        // Setup LUKS encryption for all volumes
        let containers = setup_multi_volume_encryption(
            &self.cmd,
            &self.config,
            &self.config.disk.device,
            &luks_parts,
        )?;

        self.luks_containers = containers;

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

        info!("Multi-volume encryption setup complete: {} containers", self.luks_containers.len());
        Ok(())
    }

    /// Format all partitions for multi-volume encrypted layout
    fn format_multi_volume_partitions(&self) -> Result<()> {
        info!("[Phase 2/6] Formatting multi-volume encrypted partitions");

        let layout = self.layout.as_ref().unwrap();

        // Format each LUKS-mapped device as BTRFS
        for container in &self.luks_containers {
            let label = container.mapper_name.trim_start_matches("Crypt-");
            create_btrfs_filesystem(&self.cmd, &container.mapped_path, label)?;
        }

        // Format SWAP partition
        let swap_part = layout.partitions.iter().find(|p| p.is_swap);
        if let Some(swap) = swap_part {
            let swap_device = partition_path(&self.config.disk.device, swap.number);
            format_swap(&self.cmd, &swap_device, Some("SWAP"))?;
        }

        // Format BOOT partition as BTRFS
        if let Some(ref boot_container) = self.luks_boot_container {
            format_boot(&self.cmd, &boot_container.mapped_path)?;
        } else {
            let boot_part = layout
                .partitions
                .iter()
                .find(|p| p.is_boot_fs)
                .ok_or_else(|| DeploytixError::ConfigError("No Boot partition found in layout".to_string()))?;
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

        info!("Multi-volume partitions formatted successfully");
        Ok(())
    }

    /// Mount multi-volume encrypted partitions for installation
    fn mount_multi_volume_partitions(&self) -> Result<()> {
        info!("[Phase 2/6] Mounting multi-volume encrypted partitions to {}", INSTALL_ROOT);

        let layout = self.layout.as_ref().unwrap();

        // Mount in order: root first, then usr, var, home
        // Find root container
        let root_container = self.luks_containers
            .iter()
            .find(|c| c.mapper_name == "Crypt-Root")
            .ok_or_else(|| DeploytixError::ConfigError("No Crypt-Root container found".to_string()))?;

        // Mount root
        if !self.cmd.is_dry_run() {
            fs::create_dir_all(INSTALL_ROOT)?;
        }
        self.cmd.run("mount", &[&root_container.mapped_path, INSTALL_ROOT])?;
        info!("Mounted {} to {}", root_container.mapped_path, INSTALL_ROOT);

        // Mount other encrypted volumes
        for container in &self.luks_containers {
            if container.mapper_name == "Crypt-Root" {
                continue; // Already mounted
            }

            let mount_name = container.mapper_name
                .trim_start_matches("Crypt-")
                .to_lowercase();
            let mount_point = format!("{}/{}", INSTALL_ROOT, mount_name);

            if !self.cmd.is_dry_run() {
                fs::create_dir_all(&mount_point)?;
            }
            self.cmd.run("mount", &[&container.mapped_path, &mount_point])?;
            info!("Mounted {} to {}", container.mapped_path, mount_point);
        }

        // Mount BOOT partition
        let boot_source = if let Some(ref boot_container) = self.luks_boot_container {
            boot_container.mapped_path.clone()
        } else {
            let boot_part = layout
                .partitions
                .iter()
                .find(|p| p.is_boot_fs)
                .ok_or_else(|| DeploytixError::ConfigError("No Boot partition found in layout".to_string()))?;
            partition_path(&self.config.disk.device, boot_part.number)
        };
        let boot_mount = format!("{}/boot", INSTALL_ROOT);

        if !self.cmd.is_dry_run() {
            fs::create_dir_all(&boot_mount)?;
        }
        self.cmd.run("mount", &[&boot_source, &boot_mount])?;
        info!("Mounted {} to {}", boot_source, boot_mount);

        // Mount EFI partition
        let efi_part = layout
            .partitions
            .iter()
            .find(|p| p.is_efi)
            .ok_or_else(|| DeploytixError::ConfigError("No EFI partition found in layout".to_string()))?;
        let efi_device = partition_path(&self.config.disk.device, efi_part.number);
        let efi_mount = format!("{}/boot/efi", INSTALL_ROOT);

        if !self.cmd.is_dry_run() {
            fs::create_dir_all(&efi_mount)?;
        }
        self.cmd.run("mount", &[&efi_device, &efi_mount])?;
        info!("Mounted {} to {}", efi_device, efi_mount);

        Ok(())
    }

    /// Setup keyfiles for automatic unlocking
    fn setup_keyfiles(&mut self) -> Result<()> {
        info!("[Phase 3/6] Setting up keyfiles for automatic unlocking");

        let password = self.config
            .disk
            .encryption_password
            .as_ref()
            .ok_or_else(|| DeploytixError::ValidationError(
                "Encryption password required for keyfile setup".to_string()
            ))?;

        // Collect all containers that need keyfiles (data volumes + optional boot)
        let mut all_containers: Vec<LuksContainer> = self.luks_containers.clone();
        if let Some(ref boot_container) = self.luks_boot_container {
            all_containers.push(boot_container.clone());
        }

        let keyfiles = setup_keyfiles_for_volumes(
            &self.cmd,
            &all_containers,
            password,
            INSTALL_ROOT,
        )?;

        self.keyfiles = keyfiles;
        info!("Keyfiles created for {} volumes", all_containers.len());
        Ok(())
    }

    /// Generate fstab for multi-volume encrypted system
    fn generate_fstab_multi_volume(&self) -> Result<()> {
        info!("[Phase 3/6] Generating /etc/fstab for multi-volume encrypted system");

        let layout = self.layout.as_ref().unwrap();

        generate_fstab_multi_volume(
            &self.cmd,
            &self.luks_containers,
            &self.config.disk.device,
            layout,
            INSTALL_ROOT,
        )
    }

    /// Generate crypttab for multi-volume encrypted system
    fn generate_crypttab_multi_volume(&self) -> Result<()> {
        info!("[Phase 3/6] Generating /etc/crypttab for multi-volume encrypted system");

        generate_crypttab_multi_volume(
            &self.cmd,
            &self.luks_containers,
            self.luks_boot_container.as_ref(),
            &self.keyfiles,
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
