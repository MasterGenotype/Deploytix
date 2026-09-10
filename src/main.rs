//! Deploytix - Automated Artix Linux Deployment Installer
//!
//! A portable CLI tool for deploying Artix Linux to removable media and disks.

use clap::{Parser, Subcommand};
use deploytix::utils::error::Result;
use tracing::{info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

// Consume from the `deploytix` library crate rather than redeclaring
// every module in this binary. This avoids compiling the entire source
// tree twice and lets the library's public API surface (e.g. the
// `pkgdeps` types referenced by `tests/*_integration.rs`) count as
// genuinely used, instead of being flagged as dead code in the binary's
// private copy of the module tree.
use deploytix::config::DeploymentConfig;
use deploytix::utils::error::DeploytixError;
use deploytix::{cleanup, config, desktop, disk, install, resources};
use pkgdeps::cli as deps_cli;

#[derive(clap::Args, Debug, Clone, Default)]
struct DepsCommonArgs {
    /// Path to an alternate pacman.conf
    #[arg(long)]
    config: Option<String>,
    /// Path to an alternate pacman database directory (e.g. /mnt/var/lib/pacman)
    #[arg(long)]
    dbpath: Option<String>,
    /// Path to an alternate root (chroot-style planning)
    #[arg(long)]
    root: Option<String>,
    /// Include optdepends in the closure / output
    #[arg(long)]
    include_optional: bool,
    /// Include makedepends
    #[arg(long)]
    include_make: bool,
    /// Include checkdepends
    #[arg(long)]
    include_check: bool,
    /// Emit JSON output
    #[arg(long)]
    json: bool,
    /// Emit Graphviz DOT output (overridden by --json for json-capable commands)
    #[arg(long)]
    dot: bool,
    /// Also resolve against the AUR (makes network requests)
    #[arg(long)]
    aur: bool,
    /// Use an offline JSON fixture instead of pacman (for CI / sandboxes)
    #[arg(long)]
    offline: Option<String>,
}

impl DepsCommonArgs {
    fn into_args(self) -> deps_cli::DepsArgs {
        deps_cli::DepsArgs {
            config: self.config,
            dbpath: self.dbpath,
            root: self.root,
            include_optional: self.include_optional,
            include_make: self.include_make,
            include_check: self.include_check,
            json: self.json,
            dot: self.dot,
            offline: self.offline,
            aur: self.aur,
        }
    }
}

#[derive(Subcommand, Debug, Clone)]
enum RehearseCommand {
    /// Rehearse a transactional update, then discard what it staged
    Update {
        /// Extra packages to install on top of a full sync/upgrade
        #[arg(trailing_var_arg = true)]
        packages: Vec<String>,
    },
    /// Rehearse a transactional removal, then discard what it staged
    Remove {
        /// Packages to remove
        #[arg(required = true, trailing_var_arg = true)]
        packages: Vec<String>,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum DepsCommand {
    /// Resolve the full dependency closure of a package
    Resolve {
        package: String,
        #[command(flatten)]
        common: DepsCommonArgs,
    },
    /// Print the dependency tree for a package
    Tree {
        package: String,
        #[command(flatten)]
        common: DepsCommonArgs,
    },
    /// List packages that require the target (reverse deps)
    Reverse {
        package: String,
        #[command(flatten)]
        common: DepsCommonArgs,
    },
    /// Render the dependency graph as Graphviz DOT
    Graph {
        package: String,
        /// Output file (default: stdout)
        #[arg(short, long)]
        output: Option<String>,
        #[command(flatten)]
        common: DepsCommonArgs,
    },
    /// Show what `pacman -S --print` would install for a package
    PlanInstall {
        package: String,
        /// Plan against a clean root (chroot-style; ignores already-installed)
        #[arg(long)]
        clean_root: bool,
        #[command(flatten)]
        common: DepsCommonArgs,
    },
    /// Print full normalized metadata for a package
    Metadata {
        package: String,
        #[command(flatten)]
        common: DepsCommonArgs,
    },
    /// Diff two packages' metadata
    Compare {
        a: String,
        b: String,
        #[command(flatten)]
        common: DepsCommonArgs,
    },
}

#[derive(Parser)]
#[command(name = "deploytix")]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Preview actions without changing the system (dry-run). Applies to
    /// `update` and `rollback`.
    #[arg(short = 'n', long, global = true)]
    dry_run: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start interactive installation wizard
    Install {
        /// Path to configuration file
        #[arg(short, long)]
        config: Option<String>,

        /// Target disk device (e.g., /dev/sda)
        #[arg(short, long)]
        device: Option<String>,

        /// Review every pacman/basestrap/yay invocation interactively
        /// before it runs, and prompt for extra packages at the end of
        /// the install.  Defaults to ON when no `--config` is supplied
        /// (so the wizard install gets reviewed) and OFF when `--config`
        /// is set (so automated runs stay silent).
        #[arg(short, long)]
        interactive: bool,

        /// Disable interactive review even when no config is supplied.
        /// Mutually exclusive with `--interactive`.
        #[arg(long, conflicts_with = "interactive")]
        no_interactive: bool,

        /// Recovery install: keep the existing /home volume instead of
        /// recreating it. Everything else on the disk is still erased.
        #[arg(long)]
        reuse_home: bool,

        /// Keyfile on THIS host that unlocks the existing HOME LUKS
        /// container. Implies --reuse-home.
        #[arg(long)]
        home_keyfile: Option<String>,
    },

    /// List available disks for installation
    ListDisks {
        /// Show all block devices, not just suitable targets
        #[arg(short, long)]
        all: bool,
    },

    /// Inspect the partition table already on a disk, and show what a
    /// home-preserving recovery install would keep versus destroy
    Inspect {
        /// Target disk device (e.g. /dev/sda)
        device: String,

        /// Check that this keyfile unlocks the existing HOME container,
        /// without opening or modifying it
        #[arg(long)]
        home_keyfile: Option<String>,
    },

    /// Validate a configuration file
    Validate {
        /// Path to configuration file
        config: String,
    },

    /// Generate a sample configuration file
    GenerateConfig {
        /// Output path for configuration file
        #[arg(short, long, default_value = "deploytix.toml")]
        output: String,
    },

    /// Cleanup: unmount partitions and optionally wipe disk
    Cleanup {
        /// Target disk device
        #[arg(short, long)]
        device: Option<String>,

        /// Wipe partition table after unmounting
        #[arg(short, long)]
        wipe: bool,
    },

    /// Rehearse an operation for real, recording every command, then undo it:
    /// an install is wiped from the disk, an update or removal is staged and
    /// then discarded
    Rehearse {
        /// What to rehearse (default: install)
        #[command(subcommand)]
        operation: Option<RehearseCommand>,

        /// Path to configuration file (install only)
        #[arg(short, long, default_value = "deploytix.toml")]
        config: String,

        /// Path to write the detailed rehearsal log file
        #[arg(short, long, default_value = "rehearsal.log")]
        log_file: String,
    },

    /// Query Artix/Arch package dependency metadata via pacman / libalpm
    Deps {
        #[command(subcommand)]
        action: DepsCommand,
    },

    /// Transactional system update (immutable root): build a new snapshot set,
    /// upgrade inside it, and activate it on reboot.
    Update {
        /// Extra packages to install on top of a full sync/upgrade.
        #[arg(trailing_var_arg = true)]
        packages: Vec<String>,

        /// Number of previous snapshot sets to keep when pruning.
        #[arg(long, default_value_t = 3)]
        keep: usize,

        /// Reboot automatically once the update is staged.
        #[arg(long)]
        reboot: bool,
    },

    /// Build and install AUR packages (immutable root): compile them into a
    /// new snapshot set and activate it on reboot.
    Aur {
        /// AUR package names to build.
        #[arg(required = true, trailing_var_arg = true)]
        packages: Vec<String>,

        /// Number of previous snapshot sets to keep when pruning.
        #[arg(long, default_value_t = 3)]
        keep: usize,

        /// Reboot automatically once the build is staged.
        #[arg(long)]
        reboot: bool,
    },

    /// Transactionally remove packages (immutable root): build a new snapshot
    /// set without them and activate it on reboot.
    Remove {
        /// Packages to remove.
        #[arg(required = true, trailing_var_arg = true)]
        packages: Vec<String>,

        /// Also remove every package that depends on these (pacman -Rc).
        /// Cascades into base packages faster than most people expect.
        #[arg(long)]
        cascade: bool,

        /// Also delete configuration files instead of leaving .pacsave
        /// (pacman -Rn).
        #[arg(long)]
        purge: bool,

        /// Do not prompt for confirmation of the resolved removal set.
        #[arg(short = 'y', long)]
        yes: bool,

        /// Number of previous snapshot sets to keep when pruning.
        #[arg(long, default_value_t = 3)]
        keep: usize,

        /// Reboot automatically once the removal is staged.
        #[arg(long)]
        reboot: bool,
    },

    /// Roll back to a previous snapshot set (immutable root).
    Rollback {
        /// Snapshot set id to roll back to, or `@` for the base install.
        /// Omit to step back one set from the current one.
        target: Option<String>,

        /// List available rollback targets and exit.
        #[arg(long)]
        list: bool,

        /// Reboot automatically once the rollback is staged.
        #[arg(long)]
        reboot: bool,
    },

    /// Generate desktop file for the GUI launcher
    GenerateDesktopFile {
        /// Desktop environment (kde, gnome, xfce, none)
        #[arg(short, long)]
        de: Option<String>,

        /// Binary directory path (default: $HOME/.local/bin)
        #[arg(short, long)]
        bindir: Option<String>,

        /// Output path for desktop file
        #[arg(short, long, default_value = "deploytix-gui.desktop")]
        output: String,
    },
}

fn init_logging(verbose: bool) {
    let filter = if verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info")
    };

    tracing_subscriber::registry()
        .with(fmt::layer().with_target(false))
        .with(filter)
        .init();
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    // Start looping theme music (runs in background; stops when handle drops)
    let _audio = resources::audio::play_theme_loop();

    match cli.command {
        Some(Commands::Install {
            config,
            device,
            interactive,
            no_interactive,
            reuse_home,
            home_keyfile,
        }) => {
            // Activation: explicit flag wins; otherwise interactive ON
            // when no config file is supplied, OFF when -c is given.
            let interactive_resolved = if no_interactive {
                false
            } else if interactive {
                true
            } else {
                config.is_none()
            };
            cmd_install(
                config,
                device,
                interactive_resolved,
                RecoveryOverrides {
                    reuse_home,
                    home_keyfile,
                },
            )?;
        }
        Some(Commands::ListDisks { all }) => {
            cmd_list_disks(all)?;
        }
        Some(Commands::Inspect {
            device,
            home_keyfile,
        }) => {
            cmd_inspect(&device, home_keyfile.as_deref())?;
        }
        Some(Commands::Validate { config }) => {
            cmd_validate(&config)?;
        }
        Some(Commands::GenerateConfig { output }) => {
            cmd_generate_config(&output)?;
        }
        Some(Commands::Cleanup { device, wipe }) => {
            cmd_cleanup(device, wipe)?;
        }
        Some(Commands::Rehearse {
            operation,
            config,
            log_file,
        }) => {
            cmd_rehearse(operation, &config, &log_file)?;
        }
        Some(Commands::Deps { action }) => {
            cmd_deps(action)?;
        }
        Some(Commands::Update {
            packages,
            keep,
            reboot,
        }) => {
            cmd_update(packages, keep, reboot, cli.dry_run)?;
        }
        Some(Commands::Aur {
            packages,
            keep,
            reboot,
        }) => {
            cmd_aur(packages, keep, reboot, cli.dry_run)?;
        }

        Some(Commands::Remove {
            packages,
            cascade,
            purge,
            yes,
            keep,
            reboot,
        }) => {
            cmd_remove(packages, cascade, purge, yes, keep, reboot, cli.dry_run)?;
        }
        Some(Commands::Rollback {
            target,
            list,
            reboot,
        }) => {
            cmd_rollback(target, list, reboot, cli.dry_run)?;
        }
        Some(Commands::GenerateDesktopFile { de, bindir, output }) => {
            cmd_generate_desktop_file(de, bindir, output)?;
        }
        None => {
            // Default: run interactive wizard with full interactive review
            cmd_install(
                None,
                None,
                true,
                RecoveryOverrides {
                    reuse_home: false,
                    home_keyfile: None,
                },
            )?;
        }
    }

    Ok(())
}

/// Recovery-install options supplied on the command line.
///
/// Applied on top of whatever the config file says, so a stored config can
/// be reused for both an ordinary and a recovery install.
struct RecoveryOverrides {
    reuse_home: bool,
    home_keyfile: Option<String>,
}

impl RecoveryOverrides {
    /// Fold these flags into a loaded configuration.
    ///
    /// `--home-keyfile` implies `--reuse-home`: supplying the credential for
    /// a volume you did not ask to preserve is never what was meant, and the
    /// alternative is a silently ordinary install that erases it.
    fn apply(self, config: &mut DeploymentConfig) {
        if self.reuse_home {
            config.disk.recovery.reuse_home = true;
        }
        if let Some(keyfile) = self.home_keyfile {
            config.disk.recovery.reuse_home = true;
            config.disk.recovery.home_keyfile = Some(keyfile);
        }
    }
}

fn cmd_install(
    config_path: Option<String>,
    device: Option<String>,
    interactive: bool,
    recovery: RecoveryOverrides,
) -> Result<()> {
    use install::Installer;

    // Check for root privileges
    if !nix::unistd::geteuid().is_root() {
        return Err(DeploytixError::NotRoot);
    }

    // Load or create configuration
    let config = if let Some(path) = config_path {
        info!("Loading configuration from {}", path);
        DeploymentConfig::from_file(&path)?
    } else {
        info!("Starting interactive configuration wizard");
        DeploymentConfig::from_wizard(device)?
    };

    let mut config = config;
    recovery.apply(&mut config);

    // Validate configuration
    config.validate()?;
    print_config_warnings(&config);

    // Run installation
    let mut installer = Installer::new(config, false);
    if interactive {
        use std::sync::Arc;
        let policy = Arc::new(deploytix::utils::cli_policy::CliInteractivePolicy::new());
        installer = installer.with_policy(policy);
        info!("Interactive mode ON — pacman commands will be reviewed before running");
    }
    installer.run()?;

    Ok(())
}

/// `deploytix update` — transactional system update.
fn cmd_update(packages: Vec<String>, keep: usize, reboot: bool, dry_run: bool) -> Result<()> {
    use deploytix::immutable::backend;
    use deploytix::immutable::update::UpdateOptions;
    use deploytix::utils::command::CommandRunner;

    if !dry_run && !nix::unistd::geteuid().is_root() {
        return Err(DeploytixError::NotRoot);
    }
    let cmd = CommandRunner::new(dry_run);
    // An update is a full `pacman` transaction in a chroot — as long and as
    // input-free as an install, so it gets the same idle inhibitors.
    let _awake = (!dry_run).then(|| deploytix::utils::idle::keep_awake("Updating Artix Linux"));
    backend::active().update(
        &cmd,
        &packages,
        &UpdateOptions {
            keep_sets: keep,
            reboot,
        },
    )?;
    Ok(())
}

/// `deploytix aur` — build AUR packages into a new snapshot set.
///
/// The GUI's AUR tab is the same operation; both call `run_aur_install`, and
/// both take their refusal from `refusal_reason`, so a system that cannot build
/// says the same thing either way.
fn cmd_aur(packages: Vec<String>, keep: usize, reboot: bool, dry_run: bool) -> Result<()> {
    use deploytix::aur::capability;
    use deploytix::aur::install::{run_aur_install, AurInstallOptions};
    use deploytix::utils::command::CommandRunner;

    let cmd = CommandRunner::new(dry_run);
    let cap = capability::probe(&cmd, "");

    println!("AUR build");
    if let (Some(h), Some(u)) = (&cap.helper, &cap.build_user) {
        println!(
            "  helper:     {h}\n  build user: {} ({})",
            u.name,
            u.source.label()
        );
    }
    for blocker in cap.blockers() {
        eprintln!("  blocked:    {blocker}");
    }

    run_aur_install(
        &cmd,
        &cap,
        &packages,
        &AurInstallOptions {
            keep_sets: keep,
            reboot,
        },
    )?;
    println!("Staged. Reboot to activate.");
    Ok(())
}

/// `deploytix remove` — transactional package removal.
///
/// Whether the live backend can do this at all is the backend's own answer
/// (`remove_refusal`), not something this layer knows: today the A/B backend
/// turns it down.
fn cmd_remove(
    packages: Vec<String>,
    cascade: bool,
    purge: bool,
    yes: bool,
    keep: usize,
    reboot: bool,
    dry_run: bool,
) -> Result<()> {
    use deploytix::immutable::backend;
    use deploytix::immutable::remove::RemoveOptions;
    use deploytix::utils::command::CommandRunner;

    if !dry_run && !nix::unistd::geteuid().is_root() {
        return Err(DeploytixError::NotRoot);
    }
    let backend = backend::active();
    if let Some(reason) = backend.remove_refusal() {
        return Err(DeploytixError::ConfigError(reason.to_string()));
    }

    let cmd = CommandRunner::new(dry_run);
    // A removal is a pacman transaction in a chroot plus an initramfs
    // regeneration — the same shape as an update, so the same idle inhibitors.
    let _awake = (!dry_run).then(|| deploytix::utils::idle::keep_awake("Removing packages"));
    backend.remove(
        &cmd,
        &packages,
        &RemoveOptions {
            keep_sets: keep,
            reboot,
            cascade,
            purge,
            assume_yes: yes,
        },
    )?;
    Ok(())
}

/// `deploytix rollback` — return to a previous snapshot set (btrfs) or the other
/// slot (LVM A/B).
fn cmd_rollback(target: Option<String>, list: bool, reboot: bool, dry_run: bool) -> Result<()> {
    use deploytix::immutable::backend;
    use deploytix::utils::command::CommandRunner;

    let cmd = CommandRunner::new(dry_run);
    let backend = backend::active();

    // Listing is read-only: it returns before the root check and before any
    // idle inhibitor touches the host's power management.
    if list {
        backend.print_rollback_targets(&cmd)?;
        return Ok(());
    }
    if !dry_run && !nix::unistd::geteuid().is_root() {
        return Err(DeploytixError::NotRoot);
    }
    let _awake = (!dry_run).then(|| deploytix::utils::idle::keep_awake("Rolling back Artix Linux"));
    backend.rollback(&cmd, target.as_deref(), reboot)?;
    Ok(())
}

fn cmd_list_disks(all: bool) -> Result<()> {
    use disk::detection::list_block_devices;

    let devices = list_block_devices(all)?;

    if devices.is_empty() {
        println!("No suitable disks found.");
        return Ok(());
    }

    println!("{:<15} {:>10} {:<20} TYPE", "DEVICE", "SIZE", "MODEL");
    println!("{}", "-".repeat(60));

    for dev in devices {
        println!(
            "{:<15} {:>10} {:<20} {}",
            dev.path,
            dev.size_human(),
            dev.model.as_deref().unwrap_or("-"),
            dev.device_type
        );
    }

    Ok(())
}

/// Report the partition table already on a device.
///
/// Read-only: nothing here writes to the disk, opens a LUKS container, or
/// mounts anything. It exists so a home-preserving recovery install can be
/// rehearsed — confirm the right HOME partition is identified, and confirm
/// the keyfile actually unlocks it — before any install is started.
fn cmd_inspect(device: &str, home_keyfile: Option<&str>) -> Result<()> {
    use deploytix::configure::encryption::{verify_luks_credential, Credential};
    use deploytix::disk::detection::human_bytes;
    use deploytix::disk::existing::{find_home_partition, read_partition_table, HomeMatch};
    use deploytix::utils::command::CommandRunner;

    let table = read_partition_table(device)?;

    println!(
        "Partition table on {} ({}, {}-byte sectors)\n",
        table.device, table.label, table.sector_size
    );
    println!(
        "{:<3} {:<16} {:>12} {:>14} {:>9}  {:<13} NAME",
        "#", "DEVICE", "START", "SECTORS", "SIZE", "FS"
    );
    println!("{}", "-".repeat(80));

    let home = find_home_partition(&table);
    let home_node = match &home {
        HomeMatch::Found(p) => Some(p.node.clone()),
        _ => None,
    };

    for part in &table.partitions {
        let marker = if Some(&part.node) == home_node.as_ref() {
            "  <- home"
        } else {
            ""
        };
        println!(
            "{:<3} {:<16} {:>12} {:>14} {:>9}  {:<13} {}{}",
            part.number,
            part.node,
            part.start_sector,
            part.size_sectors,
            human_bytes(part.size_bytes(table.sector_size)),
            part.fs_type.as_deref().unwrap_or("-"),
            part.name.as_deref().unwrap_or("-"),
            marker,
        );
    }

    println!();
    match &home {
        HomeMatch::Found(p) => {
            println!(
                "A recovery install would PRESERVE {} ({}, {}) and ERASE everything else on {}.",
                p.node,
                human_bytes(p.size_bytes(table.sector_size)),
                p.fs_type.as_deref().unwrap_or("unknown filesystem"),
                table.device,
            );
        }
        HomeMatch::NotFound => {
            println!(
                "No HOME partition found on {}. A recovery install cannot run against \
                 this disk without one.",
                table.device
            );
        }
        HomeMatch::Ambiguous(candidates) => {
            let nodes: Vec<&str> = candidates.iter().map(|p| p.node.as_str()).collect();
            println!(
                "Ambiguous: {} partitions are named HOME ({}). A recovery install will \
                 refuse to guess between them.",
                candidates.len(),
                nodes.join(", "),
            );
        }
    }

    // Credential check. Deliberately the last thing reported, and never a
    // reason to skip printing the table: knowing which partition was
    // identified matters even when the keyfile is wrong.
    if let Some(keyfile) = home_keyfile {
        println!();
        let HomeMatch::Found(part) = &home else {
            eprintln!(
                "⚠  Cannot test {}: no single HOME partition identified.",
                keyfile
            );
            return Ok(());
        };
        if !part.is_luks() {
            eprintln!(
                "⚠  {} is not a LUKS container ({}); a keyfile does not apply.",
                part.node,
                part.fs_type.as_deref().unwrap_or("no filesystem signature"),
            );
            return Ok(());
        }
        let cmd = CommandRunner::new(false);
        let credential = Credential::Keyfile(keyfile.to_string());
        match verify_luks_credential(&cmd, &part.node, &credential) {
            Ok(()) => println!("✓ {} unlocks {}", credential.describe(), part.node),
            Err(e) => {
                eprintln!("✗ {} does not unlock {}", credential.describe(), part.node);
                return Err(e);
            }
        }
    }

    Ok(())
}

fn cmd_validate(config_path: &str) -> Result<()> {
    let config = DeploymentConfig::from_file(config_path)?;
    config.validate()?;
    println!("✓ Configuration is valid");
    print_config_warnings(&config);
    Ok(())
}

/// Print the config's non-fatal advisories (see `DeploymentConfig::warnings`).
///
/// These do not block the install — they flag choices that produce a working
/// system but a poor first boot.
fn print_config_warnings(config: &DeploymentConfig) {
    for warning in config.warnings() {
        warn!("{}", warning);
        eprintln!("⚠  {}", warning);
    }
}

fn cmd_generate_config(output: &str) -> Result<()> {
    let sample = DeploymentConfig::sample();
    let content = toml::to_string_pretty(&sample)?;
    std::fs::write(output, content)?;
    println!("✓ Sample configuration written to {}", output);
    Ok(())
}

fn cmd_rehearse(
    operation: Option<RehearseCommand>,
    config_path: &str,
    log_file: &str,
) -> Result<()> {
    use deploytix::rehearsal::{run_rehearsal, RehearsalOp};

    // Rehearsal acts on the real system — must be root
    if !nix::unistd::geteuid().is_root() {
        return Err(DeploytixError::NotRoot);
    }

    let op = match operation {
        None => {
            let config = DeploymentConfig::from_file(config_path)?;
            config.validate()?;
            eprintln!(
                "⚠  REHEARSAL MODE: this will write to {} for real, then WIPE the disk.",
                config.disk.device
            );
            eprintln!("   All data on the target device will be destroyed.\n");
            RehearsalOp::Install(Box::new(config))
        }
        Some(RehearseCommand::Update { packages }) => {
            eprintln!(
                "⚠  REHEARSAL MODE: this stages a real update against this system, \
                 then discards it."
            );
            eprintln!("   Nothing is activated, but the transaction runs for real.\n");
            RehearsalOp::Update(packages)
        }
        Some(RehearseCommand::Remove { packages }) => {
            eprintln!(
                "⚠  REHEARSAL MODE: this stages a real removal against this system, \
                 then discards it."
            );
            eprintln!("   Nothing is activated, but the transaction runs for real.\n");
            RehearsalOp::Remove(packages)
        }
    };

    let report = run_rehearsal(op);
    report.print_table();

    // Write detailed log
    if let Err(e) = report.write_to_file(std::path::Path::new(log_file)) {
        eprintln!("Warning: failed to write log file {}: {}", log_file, e);
    } else {
        eprintln!("Detailed log written to {}", log_file);
    }

    if report.has_failures() {
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_cleanup(device: Option<String>, wipe: bool) -> Result<()> {
    use cleanup::Cleaner;

    if !nix::unistd::geteuid().is_root() {
        return Err(DeploytixError::NotRoot);
    }

    let cleaner = Cleaner::new(false);
    cleaner.cleanup(device.as_deref(), wipe)?;

    Ok(())
}

fn cmd_generate_desktop_file(
    de: Option<String>,
    bindir: Option<String>,
    output: String,
) -> Result<()> {
    use config::DesktopEnvironment;
    use desktop::generate_desktop_file;

    // Detect desktop environment if not specified
    let desktop_env = if let Some(de_str) = de {
        match de_str.to_lowercase().as_str() {
            "kde" | "plasma" => DesktopEnvironment::Kde,
            "gnome" => DesktopEnvironment::Gnome,
            "xfce" => DesktopEnvironment::Xfce,
            "none" => DesktopEnvironment::None,
            _ => {
                return Err(DeploytixError::ConfigError(format!(
                    "Unknown desktop environment: {}. Valid options: kde, gnome, xfce, none",
                    de_str
                )))
            }
        }
    } else {
        // Auto-detect desktop environment
        detect_desktop_environment()
    };

    // Determine bindir (default to $HOME/.local/bin)
    let bindir_path = if let Some(path) = bindir {
        path
    } else {
        let home = std::env::var("HOME")
            .unwrap_or_else(|_| std::env::var("USERPROFILE").unwrap_or_else(|_| ".".to_string()));
        format!("{}/.local/bin", home)
    };

    // Generate desktop file content
    let content = generate_desktop_file(&desktop_env, &bindir_path);

    // Write to file
    std::fs::write(&output, content)?;
    println!("✓ Desktop file generated for {} at {}", desktop_env, output);

    Ok(())
}

/// The metadata source for a `deploytix deps` invocation.
///
/// `pkgdeps` builds the local source (pacman, or an offline fixture); stacking
/// the AUR on top happens here, because knowing that the AUR exists is this
/// layer's business, not the dependency resolver's.
fn deps_source(args: &deps_cli::DepsArgs) -> Result<Box<dyn pkgdeps::source::MetadataSource>> {
    use deploytix::aur::rpc::CurlGet;
    use deploytix::aur::source::{AurSource, CompositeSource};

    let local = deps_cli::build_source(args)?;
    if deps_cli::wants_aur(args) {
        Ok(Box::new(CompositeSource::new(
            local,
            AurSource::new(CurlGet),
        )))
    } else {
        Ok(local)
    }
}

fn cmd_deps(action: DepsCommand) -> Result<()> {
    match action {
        DepsCommand::Resolve { package, common } => {
            let args = common.into_args();
            let source = deps_source(&args)?;
            deps_cli::cmd_resolve(source.as_ref(), &package, &args)?;
        }
        DepsCommand::Tree { package, common } => {
            let args = common.into_args();
            let source = deps_source(&args)?;
            deps_cli::cmd_tree(source.as_ref(), &package, &args)?;
        }
        DepsCommand::Reverse { package, common } => {
            let args = common.into_args();
            let source = deps_source(&args)?;
            deps_cli::cmd_reverse(source.as_ref(), &package, &args)?;
        }
        DepsCommand::Graph {
            package,
            output,
            common,
        } => {
            let args = common.into_args();
            let source = deps_source(&args)?;
            deps_cli::cmd_graph(source.as_ref(), &package, output.as_deref(), &args)?;
        }
        DepsCommand::PlanInstall {
            package,
            clean_root,
            common,
        } => {
            let args = common.into_args();
            let source = deps_source(&args)?;
            deps_cli::cmd_plan_install(source.as_ref(), &package, clean_root, &args)?;
        }
        DepsCommand::Metadata { package, common } => {
            let args = common.into_args();
            let source = deps_source(&args)?;
            deps_cli::cmd_metadata(source.as_ref(), &package, &args)?;
        }
        DepsCommand::Compare { a, b, common } => {
            let args = common.into_args();
            let source = deps_source(&args)?;
            deps_cli::cmd_compare(source.as_ref(), &a, &b, &args)?;
        }
    }
    Ok(())
}

/// Auto-detect the current desktop environment
fn detect_desktop_environment() -> config::DesktopEnvironment {
    // Check environment variables
    if let Ok(desktop) = std::env::var("XDG_CURRENT_DESKTOP") {
        let desktop_lower = desktop.to_lowercase();
        if desktop_lower.contains("kde") || desktop_lower.contains("plasma") {
            info!("Detected KDE Plasma desktop environment");
            return config::DesktopEnvironment::Kde;
        } else if desktop_lower.contains("gnome") {
            info!("Detected GNOME desktop environment");
            return config::DesktopEnvironment::Gnome;
        } else if desktop_lower.contains("xfce") {
            info!("Detected XFCE desktop environment");
            return config::DesktopEnvironment::Xfce;
        }
    }

    // Check for KDE session
    if std::env::var("KDE_FULL_SESSION").is_ok() {
        info!("Detected KDE session");
        return config::DesktopEnvironment::Kde;
    }

    // Check for GNOME session
    if std::env::var("GNOME_DESKTOP_SESSION_ID").is_ok()
        || std::env::var("GNOME_SHELL_SESSION_MODE").is_ok()
    {
        info!("Detected GNOME session");
        return config::DesktopEnvironment::Gnome;
    }

    // Default to None if not detected
    info!("Could not detect desktop environment, using generic desktop file");
    config::DesktopEnvironment::None
}
