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
mod resources;
mod utils;

use crate::config::DeploymentConfig;
use crate::utils::error::DeploytixError;

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
