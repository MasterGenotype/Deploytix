//! Main installation orchestrator

use crate::config::{DeploymentConfig, PartitionLayout, SwapType};
use crate::configure;
use crate::configure::encryption::{
    close_multi_luks, setup_multi_volume_encryption, LuksContainer,
};
use crate::configure::keyfiles::{setup_keyfiles_for_volumes, VolumeKeyfile};
use crate::desktop;
use crate::disk::detection::{get_device_info, partition_path};
use crate::disk::formatting::{
    create_btrfs_filesystem, format_all_partitions, format_boot, format_efi, format_swap,
};
use crate::disk::layouts::{
    compute_layout, compute_lvm_thin_layout_with_swap, get_luks_partitions, print_layout_summary,
    ComputedLayout,
};
use crate::disk::lvm::{self, lv_path, ThinVolumeDef};
use crate::disk::partitioning::apply_partitions;
use crate::install::crypttab::generate_crypttab_multi_volume;
use crate::install::fstab::{
    append_swap_file_entry, generate_fstab_lvm_thin, generate_fstab_multi_volume,
};
use crate::install::{generate_fstab, mount_partitions, run_basestrap, unmount_all};
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use crate::utils::prompt::warn_confirm;
use std::fs;
use tracing::info;

/// Installation target path
pub const INSTALL_ROOT: &str = "/install";

/// Progress callback type for reporting installation progress.
/// Takes a value between 0.0 and 1.0, and a status message describing the current phase.
pub type ProgressCallback = Box<dyn Fn(f32, &str) + Send>;

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
    /// LVM thin volumes for LvmThin layout
    lvm_thin_volumes: Vec<ThinVolumeDef>,
    /// LUKS container for LVM PV (LvmThin layout)
    luks_lvm_container: Option<LuksContainer>,
    /// Skip interactive confirmation prompt (e.g. when GUI already confirmed)
    skip_confirm: bool,
    /// Optional progress callback for GUI integration
    progress_cb: Option<ProgressCallback>,
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
            lvm_thin_volumes: Vec::new(),
            luks_lvm_container: None,
            skip_confirm: false,
            progress_cb: None,
        }
    }

    /// Skip the interactive confirmation prompt.
    /// Use this when confirmation has already been obtained (e.g. via GUI).
    #[allow(dead_code)]
    pub fn with_skip_confirm(mut self, skip: bool) -> Self {
        self.skip_confirm = skip;
        self
    }

    /// Set a progress callback for reporting installation progress.
    /// The callback receives a progress value (0.0–1.0) and a status message.
    #[allow(dead_code)]
    pub fn with_progress_callback(mut self, cb: ProgressCallback) -> Self {
        self.progress_cb = Some(cb);
        self
    }

    /// Report progress via the callback, if one is set.
    fn report_progress(&self, progress: f32, status: &str) {
        if let Some(ref cb) = self.progress_cb {
            cb(progress, status);
        }
    }

    /// Run the full installation process
    pub fn run(mut self) -> Result<()> {
        info!(
            "Starting Deploytix installation on {} ({} layout, {} init)",
            self.config.disk.device, self.config.disk.layout, self.config.system.init
        );

        // Phase 1: Preparation
        self.report_progress(0.0, "Preparing installation...");
        self.prepare()?;

        // Phase 2: Disk operations
        self.report_progress(0.10, "Partitioning disk...");
        self.partition_disk()?;

        // Phase 2.5: LUKS + LVM/multi-volume setup
        if self.config.disk.layout == PartitionLayout::LvmThin {
            // LVM Thin Provisioning layout with LUKS encryption
            self.report_progress(0.15, "Setting up LVM thin provisioning...");
            self.setup_lvm_thin()?;
            self.report_progress(0.22, "Formatting LVM volumes...");
            self.format_lvm_volumes()?;
            self.report_progress(0.28, "Mounting LVM volumes...");
            self.mount_lvm_volumes()?;
        } else if self.config.disk.encryption
            && self.config.disk.layout == PartitionLayout::Standard
        {
            // Multi-volume encryption: separate LUKS containers for root, usr, var, home
            self.report_progress(0.15, "Setting up encryption...");
            self.setup_multi_volume_encryption()?;
            self.report_progress(0.22, "Formatting encrypted partitions...");
            self.format_multi_volume_partitions()?;
            self.report_progress(0.28, "Mounting encrypted partitions...");
            self.mount_multi_volume_partitions()?;
        } else {
            self.report_progress(0.20, "Formatting partitions...");
            self.format_partitions()?;
            self.report_progress(0.28, "Mounting partitions...");
            self.mount_partitions()?;
        }

        // Phase 3: Base system
        self.report_progress(0.30, "Installing base system (this may take a while)...");
        self.install_base_system()?;

        // Phase 3.5: Generate fstab (different method for each layout)
        self.report_progress(0.55, "Generating fstab...");
        if self.config.disk.layout == PartitionLayout::LvmThin {
            self.generate_fstab_lvm_thin()?;
        } else if self.config.disk.encryption
            && self.config.disk.layout == PartitionLayout::Standard
        {
            self.generate_fstab_multi_volume()?;
        } else {
            self.generate_fstab()?;
            // Add swap file entry if using FileZram
            if self.config.disk.swap_type == SwapType::FileZram {
                append_swap_file_entry(INSTALL_ROOT)?;
            }
        }

        // Phase 3.6: Crypttab and keyfiles (for encrypted systems)
        if self.config.disk.encryption && self.config.disk.layout == PartitionLayout::Standard {
            self.report_progress(0.60, "Setting up keyfiles and crypttab...");
            self.setup_keyfiles()?;
            self.generate_crypttab_multi_volume()?;
        } else if self.config.disk.layout == PartitionLayout::LvmThin {
            self.report_progress(0.60, "Setting up LVM crypttab...");
            self.generate_crypttab_lvm_thin()?;
        }

        // Phase 3.7: Swap configuration (ZRAM / swap file)
        if self.config.disk.swap_type != SwapType::Partition {
            self.report_progress(0.62, "Configuring swap...");
            self.configure_swap()?;
        }

        // Phase 4: System configuration
        self.report_progress(0.65, "Configuring system...");
        self.configure_system()?;

        // Phase 4.5: Custom hooks (for encrypted systems)
        if self.config.disk.encryption
            && (self.config.disk.layout == PartitionLayout::Standard
                || self.config.disk.layout == PartitionLayout::LvmThin)
        {
            self.report_progress(0.75, "Installing custom hooks...");
            self.install_custom_hooks()?;
        }

        // Phase 4.6: SecureBoot setup (if enabled)
        if self.config.system.secureboot {
            self.report_progress(0.78, "Setting up SecureBoot...");
            self.setup_secureboot()?;
        }

        // Phase 5: Desktop environment (if selected)
        self.report_progress(0.80, "Installing desktop environment...");
        self.install_desktop()?;

        // Phase 6: Finalization
        self.report_progress(0.90, "Finalizing installation...");
        self.finalize()?;

        self.report_progress(1.0, "Installation complete");
        info!(
            "Installation to {} finished successfully",
            self.config.disk.device
        );
        println!("\n✓ Installation completed successfully!");
        println!("  You can now reboot into your new Artix Linux system.");

        Ok(())
    }

    /// Prepare for installation
    fn prepare(&mut self) -> Result<()> {
        info!(
            "[Phase 1/6] Preparing installation for {}",
            self.config.disk.device
        );

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
        // For LvmThin, use the swap-aware layout function
        let layout = if self.config.disk.layout == PartitionLayout::LvmThin {
            let use_swap_partition = self.config.disk.swap_type == SwapType::Partition;
            compute_lvm_thin_layout_with_swap(disk_mib, use_swap_partition)?
        } else {
            compute_layout(
                &self.config.disk.layout,
                disk_mib,
                self.config.disk.encryption,
            )?
        };
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
        info!(
            "[Phase 2/6] Partitioning {} with {} layout",
            self.config.disk.device, self.config.disk.layout
        );

        let layout = self.layout.as_ref().unwrap();
        apply_partitions(&self.cmd, &self.config.disk.device, layout)?;

        Ok(())
    }

    /// Format partitions
    fn format_partitions(&self) -> Result<()> {
        info!(
            "[Phase 2/6] Formatting partitions on {} as {}",
            self.config.disk.device, self.config.disk.filesystem
        );

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
        if self.config.disk.encryption
            && (self.config.disk.layout == PartitionLayout::Standard
                || self.config.disk.layout == PartitionLayout::LvmThin)
        {
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

        // Close LVM thin container if used
        if let Some(ref lvm_container) = self.luks_lvm_container {
            // Deactivate VG first
            let vg_name = &self.config.disk.lvm_vg_name;
            lvm::deactivate_vg(&self.cmd, vg_name)?;
            configure::encryption::close_luks(&self.cmd, &lvm_container.mapper_name)?;
        }

        Ok(())
    }

    // ==================== MULTI-VOLUME ENCRYPTION METHODS ====================

    /// Setup multi-volume LUKS encryption (root, usr, var, home)
    fn setup_multi_volume_encryption(&mut self) -> Result<()> {
        info!(
            "[Phase 2/6] Setting up multi-volume LUKS2 encryption on {}",
            self.config.disk.device
        );

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
                .ok_or_else(|| {
                    DeploytixError::ConfigError("No Boot partition found in layout".to_string())
                })?;

            let boot_container = configure::encryption::setup_boot_encryption(
                &self.cmd,
                &self.config,
                &self.config.disk.device,
                boot_part.number,
            )?;

            self.luks_boot_container = Some(boot_container);
        }

        info!(
            "Multi-volume encryption setup complete: {} containers",
            self.luks_containers.len()
        );
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
                .ok_or_else(|| {
                    DeploytixError::ConfigError("No Boot partition found in layout".to_string())
                })?;
            let boot_device = partition_path(&self.config.disk.device, boot_part.number);
            format_boot(&self.cmd, &boot_device)?;
        }

        // Format EFI partition as FAT32
        let efi_part = layout.partitions.iter().find(|p| p.is_efi).ok_or_else(|| {
            DeploytixError::ConfigError("No EFI partition found in layout".to_string())
        })?;
        let efi_device = partition_path(&self.config.disk.device, efi_part.number);
        format_efi(&self.cmd, &efi_device)?;

        info!("Multi-volume partitions formatted successfully");
        Ok(())
    }

    /// Mount multi-volume encrypted partitions for installation
    fn mount_multi_volume_partitions(&self) -> Result<()> {
        info!(
            "[Phase 2/6] Mounting multi-volume encrypted partitions to {}",
            INSTALL_ROOT
        );

        let layout = self.layout.as_ref().unwrap();

        // Mount in order: root first, then usr, var, home
        // Find root container
        let root_container = self
            .luks_containers
            .iter()
            .find(|c| c.mapper_name == "Crypt-Root")
            .ok_or_else(|| {
                DeploytixError::ConfigError("No Crypt-Root container found".to_string())
            })?;

        // Mount root
        if !self.cmd.is_dry_run() {
            fs::create_dir_all(INSTALL_ROOT)?;
        }
        self.cmd
            .run("mount", &[&root_container.mapped_path, INSTALL_ROOT])?;
        info!("Mounted {} to {}", root_container.mapped_path, INSTALL_ROOT);

        // Mount other encrypted volumes
        for container in &self.luks_containers {
            if container.mapper_name == "Crypt-Root" {
                continue; // Already mounted
            }

            let mount_name = container
                .mapper_name
                .trim_start_matches("Crypt-")
                .to_lowercase();
            let mount_point = format!("{}/{}", INSTALL_ROOT, mount_name);

            if !self.cmd.is_dry_run() {
                fs::create_dir_all(&mount_point)?;
            }
            self.cmd
                .run("mount", &[&container.mapped_path, &mount_point])?;
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
                .ok_or_else(|| {
                    DeploytixError::ConfigError("No Boot partition found in layout".to_string())
                })?;
            partition_path(&self.config.disk.device, boot_part.number)
        };
        let boot_mount = format!("{}/boot", INSTALL_ROOT);

        if !self.cmd.is_dry_run() {
            fs::create_dir_all(&boot_mount)?;
        }
        self.cmd.run("mount", &[&boot_source, &boot_mount])?;
        info!("Mounted {} to {}", boot_source, boot_mount);

        // Mount EFI partition
        let efi_part = layout.partitions.iter().find(|p| p.is_efi).ok_or_else(|| {
            DeploytixError::ConfigError("No EFI partition found in layout".to_string())
        })?;
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

        let password = self
            .config
            .disk
            .encryption_password
            .as_ref()
            .ok_or_else(|| {
                DeploytixError::ValidationError(
                    "Encryption password required for keyfile setup".to_string(),
                )
            })?;

        // Collect all containers that need keyfiles (data volumes + optional boot)
        let mut all_containers: Vec<LuksContainer> = self.luks_containers.clone();
        if let Some(ref boot_container) = self.luks_boot_container {
            all_containers.push(boot_container.clone());
        }

        let keyfiles =
            setup_keyfiles_for_volumes(&self.cmd, &all_containers, password, INSTALL_ROOT)?;

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
            self.config.disk.integrity,
            INSTALL_ROOT,
        )
    }

    /// Install custom mkinitcpio hooks
    fn install_custom_hooks(&self) -> Result<()> {
        let layout = self.layout.as_ref().unwrap();

        configure::hooks::install_custom_hooks(&self.cmd, &self.config, layout, INSTALL_ROOT)
    }

    // ==================== LVM THIN PROVISIONING METHODS ====================

    /// Setup LVM thin provisioning with LUKS encryption
    fn setup_lvm_thin(&mut self) -> Result<()> {
        info!(
            "[Phase 2/6] Setting up LVM thin provisioning on {}",
            self.config.disk.device
        );

        let layout = self.layout.as_ref().unwrap();
        let vg_name = &self.config.disk.lvm_vg_name;
        let pool_name = &self.config.disk.lvm_thin_pool_name;
        let pool_percent = self.config.disk.lvm_thin_pool_percent;

        // Find the LVM PV partition (marked as root for LvmThin layout - it's the large partition)
        // In LvmThin layout, the "root" partition is actually the LVM PV
        let lvm_part = layout
            .partitions
            .iter()
            .find(|p| {
                p.mount_point.as_deref() == Some("/") || p.name.to_lowercase().contains("lvm")
            })
            .ok_or_else(|| {
                DeploytixError::ConfigError("No LVM PV partition found in layout".to_string())
            })?;

        let lvm_device = partition_path(&self.config.disk.device, lvm_part.number);

        // Setup LUKS encryption on LVM PV partition
        if self.config.disk.encryption {
            let password = self
                .config
                .disk
                .encryption_password
                .as_ref()
                .ok_or_else(|| {
                    DeploytixError::ValidationError(
                        "Encryption password required for LVM thin layout".to_string(),
                    )
                })?;

            let container = if self.config.disk.integrity {
                configure::encryption::setup_single_luks_with_integrity(
                    &self.cmd,
                    &lvm_device,
                    password,
                    "Crypt-LVM",
                )?
            } else {
                configure::encryption::setup_single_luks(
                    &self.cmd,
                    &lvm_device,
                    password,
                    "Crypt-LVM",
                )?
            };

            // Create PV on the LUKS container
            lvm::create_pv(&self.cmd, &container.mapped_path)?;
            lvm::create_vg(&self.cmd, vg_name, &container.mapped_path)?;

            self.luks_lvm_container = Some(container);
        } else {
            // Create PV directly on partition
            lvm::create_pv(&self.cmd, &lvm_device)?;
            lvm::create_vg(&self.cmd, vg_name, &lvm_device)?;
        }

        // Create thin pool
        lvm::create_thin_pool(&self.cmd, vg_name, pool_name, pool_percent)?;

        // Create thin volumes using the batch function
        let thin_volumes = lvm::default_thin_volumes();
        lvm::create_all_thin_volumes(&self.cmd, vg_name, pool_name, &thin_volumes)?;

        // Activate VG to make LVs available
        lvm::activate_vg(&self.cmd, vg_name)?;

        self.lvm_thin_volumes = thin_volumes;

        info!(
            "LVM thin provisioning setup complete: VG={}, pool={}",
            vg_name, pool_name
        );
        Ok(())
    }

    /// Format LVM thin volumes as btrfs
    fn format_lvm_volumes(&self) -> Result<()> {
        info!("[Phase 2/6] Formatting LVM thin volumes");

        let layout = self.layout.as_ref().unwrap();
        let vg_name = &self.config.disk.lvm_vg_name;

        // Format each thin volume as btrfs
        for vol in &self.lvm_thin_volumes {
            let lv_device = lv_path(vg_name, &vol.name);
            create_btrfs_filesystem(&self.cmd, &lv_device, &vol.name)?;
        }

        // Format SWAP partition if present and using partition swap
        if self.config.disk.swap_type == SwapType::Partition {
            let swap_part = layout.partitions.iter().find(|p| p.is_swap);
            if let Some(swap) = swap_part {
                let swap_device = partition_path(&self.config.disk.device, swap.number);
                format_swap(&self.cmd, &swap_device, Some("SWAP"))?;
            }
        }

        // Format BOOT partition as btrfs
        let boot_part = layout
            .partitions
            .iter()
            .find(|p| p.is_boot_fs)
            .ok_or_else(|| {
                DeploytixError::ConfigError("No Boot partition found in layout".to_string())
            })?;
        let boot_device = partition_path(&self.config.disk.device, boot_part.number);
        format_boot(&self.cmd, &boot_device)?;

        // Format EFI partition as FAT32
        let efi_part = layout.partitions.iter().find(|p| p.is_efi).ok_or_else(|| {
            DeploytixError::ConfigError("No EFI partition found in layout".to_string())
        })?;
        let efi_device = partition_path(&self.config.disk.device, efi_part.number);
        format_efi(&self.cmd, &efi_device)?;

        info!("LVM thin volumes formatted successfully");
        Ok(())
    }

    /// Mount LVM thin volumes for installation
    fn mount_lvm_volumes(&self) -> Result<()> {
        info!("[Phase 2/6] Mounting LVM thin volumes to {}", INSTALL_ROOT);

        let layout = self.layout.as_ref().unwrap();
        let vg_name = &self.config.disk.lvm_vg_name;

        // Ensure VG is activated and LVs are visible
        // This is a safety measure in case the VG wasn't properly activated
        lvm::scan_and_activate(&self.cmd)?;

        // Mount root first (use lv_paths for logging both formats)
        let (root_device, root_mapper) = lvm::lv_paths(vg_name, "root");
        info!("Root LV paths: {} (or {})", root_device, root_mapper);
        if !self.cmd.is_dry_run() {
            fs::create_dir_all(INSTALL_ROOT)?;
        }
        self.cmd.run("mount", &[&root_device, INSTALL_ROOT])?;
        info!("Mounted {} to {}", root_device, INSTALL_ROOT);

        // Mount other volumes in order
        for vol in &self.lvm_thin_volumes {
            if vol.mount_point == "/" {
                continue; // Already mounted
            }

            let lv_device = lv_path(vg_name, &vol.name);
            let mount_point = format!("{}{}", INSTALL_ROOT, &vol.mount_point);

            if !self.cmd.is_dry_run() {
                fs::create_dir_all(&mount_point)?;
            }
            self.cmd.run("mount", &[&lv_device, &mount_point])?;
            info!("Mounted {} to {}", lv_device, mount_point);
        }

        // Mount BOOT partition
        let boot_part = layout
            .partitions
            .iter()
            .find(|p| p.is_boot_fs)
            .ok_or_else(|| {
                DeploytixError::ConfigError("No Boot partition found in layout".to_string())
            })?;
        let boot_device = partition_path(&self.config.disk.device, boot_part.number);
        let boot_mount = format!("{}/boot", INSTALL_ROOT);

        if !self.cmd.is_dry_run() {
            fs::create_dir_all(&boot_mount)?;
        }
        self.cmd.run("mount", &[&boot_device, &boot_mount])?;
        info!("Mounted {} to {}", boot_device, boot_mount);

        // Mount EFI partition
        let efi_part = layout.partitions.iter().find(|p| p.is_efi).ok_or_else(|| {
            DeploytixError::ConfigError("No EFI partition found in layout".to_string())
        })?;
        let efi_device = partition_path(&self.config.disk.device, efi_part.number);
        let efi_mount = format!("{}/boot/efi", INSTALL_ROOT);

        if !self.cmd.is_dry_run() {
            fs::create_dir_all(&efi_mount)?;
        }
        self.cmd.run("mount", &[&efi_device, &efi_mount])?;
        info!("Mounted {} to {}", efi_device, efi_mount);

        Ok(())
    }

    /// Generate fstab for LVM thin layout
    fn generate_fstab_lvm_thin(&self) -> Result<()> {
        info!("[Phase 3/6] Generating /etc/fstab for LVM thin layout");

        let layout = self.layout.as_ref().unwrap();
        let vg_name = &self.config.disk.lvm_vg_name;

        generate_fstab_lvm_thin(
            &self.cmd,
            vg_name,
            &self.lvm_thin_volumes,
            &self.config.disk.device,
            layout,
            &self.config.disk.swap_type,
            INSTALL_ROOT,
        )
    }

    /// Generate crypttab for LVM thin layout
    fn generate_crypttab_lvm_thin(&self) -> Result<()> {
        if let Some(ref container) = self.luks_lvm_container {
            info!("[Phase 3/6] Generating /etc/crypttab for LVM LUKS container");

            let crypttab_path = format!("{}/etc/crypttab", INSTALL_ROOT);
            if !self.cmd.is_dry_run() {
                fs::create_dir_all(format!("{}/etc", INSTALL_ROOT))?;
            }

            // Get LUKS UUID
            let luks_uuid = configure::encryption::get_luks_uuid(&container.device)?;

            let content = format!(
                "# /etc/crypttab: LUKS container for LVM thin provisioning\n\
                 # <target name>  <source device>  <key file>  <options>\n\
                 {}  UUID={}  none  luks\n",
                container.mapper_name, luks_uuid
            );

            if !self.cmd.is_dry_run() {
                fs::write(&crypttab_path, content)?;
            }
            info!("Crypttab written to {}", crypttab_path);
        }
        Ok(())
    }

    // ==================== SWAP CONFIGURATION METHODS ====================

    /// Configure swap (ZRAM and/or swap file)
    fn configure_swap(&self) -> Result<()> {
        info!(
            "[Phase 3/6] Configuring swap: {:?}",
            self.config.disk.swap_type
        );

        // Use the unified configure_swap function
        configure::swap::configure_swap(&self.cmd, &self.config, INSTALL_ROOT)
    }

    // ==================== SECUREBOOT METHODS ====================

    /// Setup SecureBoot signing
    fn setup_secureboot(&self) -> Result<()> {
        info!("[Phase 4/6] Setting up SecureBoot");

        configure::secureboot::setup_secureboot(&self.cmd, &self.config, INSTALL_ROOT)?;

        // Sign boot files
        configure::secureboot::sign_boot_files(&self.cmd, &self.config, INSTALL_ROOT)?;

        // Print enrollment instructions for user
        configure::secureboot::print_enrollment_instructions(&self.config);

        Ok(())
    }
}
