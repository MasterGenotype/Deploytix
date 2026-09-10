//! Standalone rehearsal binary for Deploytix.
//!
//! This is a thin wrapper around `deploytix::rehearsal::run_rehearsal` that
//! provides its own CLI argument parsing.  It exists so that the rehearsal
//! tool can be built and distributed as a separate binary (`deploytix-rehearsal`)
//! without needing the full CLI subcommand infrastructure.
//!
//! Install rehearsal only.  Update and removal rehearsals act on a live
//! deployed system rather than on a target disk, so they live with the rest of
//! the system-management commands: `deploytix rehearse update|remove`.

use clap::Parser;
use deploytix::config::DeploymentConfig;
use deploytix::rehearsal::{run_rehearsal, RehearsalOp};
use deploytix::utils::error::DeploytixError;
use deploytix::utils::error::Result;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[derive(Parser)]
#[command(name = "deploytix-rehearsal")]
#[command(
    about = "Run a rehearsal installation: execute the full install, record every command, then wipe the disk"
)]
#[command(version)]
struct Args {
    /// Path to configuration file
    #[arg(short, long, default_value = "deploytix.toml")]
    config: String,

    /// Path to write the detailed rehearsal log file
    #[arg(short, long, default_value = "rehearsal.log")]
    log_file: String,

    /// Enable verbose output
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Logging
    let filter = if args.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info")
    };
    tracing_subscriber::registry()
        .with(fmt::layer().with_target(false))
        .with(filter)
        .init();

    // Must be root
    if !nix::unistd::geteuid().is_root() {
        return Err(DeploytixError::NotRoot);
    }

    let config = DeploymentConfig::from_file(&args.config)?;
    config.validate()?;

    eprintln!(
        "⚠  REHEARSAL MODE: this will write to {} for real, then WIPE the disk.",
        config.disk.device
    );
    eprintln!("   All data on the target device will be destroyed.");
    eprintln!("   Live output will appear below as each operation completes.\n");

    let report = run_rehearsal(RehearsalOp::Install(Box::new(config)));
    report.print_table();

    // Write detailed log
    if let Err(e) = report.write_to_file(std::path::Path::new(&args.log_file)) {
        eprintln!("Warning: failed to write log file {}: {}", args.log_file, e);
    } else {
        eprintln!("Detailed log written to {}", args.log_file);
    }

    if report.has_failures() {
        std::process::exit(1);
    }

    Ok(())
}
