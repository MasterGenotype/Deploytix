//! Deploytix - Automated Artix Linux Deployment Installer
//!
//! A portable CLI tool for deploying Artix Linux to removable media and disks.

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::info;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

mod cleanup;
mod config;
mod configure;
mod desktop;
mod disk;
mod install;
mod pkgdeps;
mod resources;
mod utils;

use crate::config::DeploymentConfig;
use crate::pkgdeps::cli as deps_cli;
use crate::utils::error::DeploytixError;

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
        }
    }
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

    /// Dry run mode - show what would be done without making changes
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
    },

    /// List available disks for installation
    ListDisks {
        /// Show all block devices, not just suitable targets
        #[arg(short, long)]
        all: bool,
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

    /// Query Artix/Arch package dependency metadata via pacman / libalpm
    Deps {
        #[command(subcommand)]
        action: DepsCommand,
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

    let dry_run = cli.dry_run;
    if dry_run {
        info!("Running in dry-run mode - no changes will be made");
    }

    match cli.command {
        Some(Commands::Install { config, device }) => {
            cmd_install(config, device, dry_run)?;
        }
        Some(Commands::ListDisks { all }) => {
            cmd_list_disks(all)?;
        }
        Some(Commands::Validate { config }) => {
            cmd_validate(&config)?;
        }
        Some(Commands::GenerateConfig { output }) => {
            cmd_generate_config(&output)?;
        }
        Some(Commands::Cleanup { device, wipe }) => {
            cmd_cleanup(device, wipe, dry_run)?;
        }
        Some(Commands::Deps { action }) => {
            cmd_deps(action)?;
        }
        Some(Commands::GenerateDesktopFile { de, bindir, output }) => {
            cmd_generate_desktop_file(de, bindir, output)?;
        }
        None => {
            // Default: run interactive wizard
            cmd_install(None, None, dry_run)?;
        }
    }

    Ok(())
}

fn cmd_install(config_path: Option<String>, device: Option<String>, dry_run: bool) -> Result<()> {
    use crate::install::Installer;

    // Check for root privileges
    if !nix::unistd::geteuid().is_root() {
        return Err(DeploytixError::NotRoot.into());
    }

    // Load or create configuration
    let config = if let Some(path) = config_path {
        info!("Loading configuration from {}", path);
        DeploymentConfig::from_file(&path)?
    } else {
        info!("Starting interactive configuration wizard");
        DeploymentConfig::from_wizard(device)?
    };

    // Validate configuration
    config.validate()?;

    // Run installation
    let installer = Installer::new(config, dry_run);
    installer.run()?;

    Ok(())
}

fn cmd_list_disks(all: bool) -> Result<()> {
    use crate::disk::detection::list_block_devices;

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

fn cmd_validate(config_path: &str) -> Result<()> {
    let config = DeploymentConfig::from_file(config_path)?;
    config.validate()?;
    println!("✓ Configuration is valid");
    Ok(())
}

fn cmd_generate_config(output: &str) -> Result<()> {
    let sample = DeploymentConfig::sample();
    let content = toml::to_string_pretty(&sample)?;
    std::fs::write(output, content)?;
    println!("✓ Sample configuration written to {}", output);
    Ok(())
}

fn cmd_cleanup(device: Option<String>, wipe: bool, dry_run: bool) -> Result<()> {
    use crate::cleanup::Cleaner;

    if !nix::unistd::geteuid().is_root() {
        return Err(DeploytixError::NotRoot.into());
    }

    let cleaner = Cleaner::new(dry_run);
    cleaner.cleanup(device.as_deref(), wipe)?;

    Ok(())
}

fn cmd_generate_desktop_file(
    de: Option<String>,
    bindir: Option<String>,
    output: String,
) -> Result<()> {
    use crate::config::DesktopEnvironment;
    use crate::desktop::generate_desktop_file;

    // Detect desktop environment if not specified
    let desktop_env = if let Some(de_str) = de {
        match de_str.to_lowercase().as_str() {
            "kde" | "plasma" => DesktopEnvironment::Kde,
            "gnome" => DesktopEnvironment::Gnome,
            "xfce" => DesktopEnvironment::Xfce,
            "none" => DesktopEnvironment::None,
            _ => {
                return Err(anyhow::anyhow!(
                    "Unknown desktop environment: {}. Valid options: kde, gnome, xfce, none",
                    de_str
                ))
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

fn cmd_deps(action: DepsCommand) -> Result<()> {
    match action {
        DepsCommand::Resolve { package, common } => {
            let args = common.into_args();
            let source = deps_cli::build_source(&args)?;
            deps_cli::cmd_resolve(source.as_ref(), &package, &args)?;
        }
        DepsCommand::Tree { package, common } => {
            let args = common.into_args();
            let source = deps_cli::build_source(&args)?;
            deps_cli::cmd_tree(source.as_ref(), &package, &args)?;
        }
        DepsCommand::Reverse { package, common } => {
            let args = common.into_args();
            let source = deps_cli::build_source(&args)?;
            deps_cli::cmd_reverse(source.as_ref(), &package, &args)?;
        }
        DepsCommand::Graph {
            package,
            output,
            common,
        } => {
            let args = common.into_args();
            let source = deps_cli::build_source(&args)?;
            deps_cli::cmd_graph(source.as_ref(), &package, output.as_deref(), &args)?;
        }
        DepsCommand::PlanInstall {
            package,
            clean_root,
            common,
        } => {
            let args = common.into_args();
            let source = deps_cli::build_source(&args)?;
            deps_cli::cmd_plan_install(source.as_ref(), &package, clean_root, &args)?;
        }
        DepsCommand::Metadata { package, common } => {
            let args = common.into_args();
            let source = deps_cli::build_source(&args)?;
            deps_cli::cmd_metadata(source.as_ref(), &package, &args)?;
        }
        DepsCommand::Compare { a, b, common } => {
            let args = common.into_args();
            let source = deps_cli::build_source(&args)?;
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
