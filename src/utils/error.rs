//! Error types for Deploytix

use thiserror::Error;

#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum DeploytixError {
    #[error("Must be run as root")]
    NotRoot,

    #[error("Device not found: {0}")]
    DeviceNotFound(String),

    #[error("Device is not a block device: {0}")]
    NotBlockDevice(String),

    #[error("Device is currently mounted: {0}")]
    DeviceMounted(String),

    #[error("Disk too small: {size_mib}MiB < required minimum {required_mib}MiB")]
    DiskTooSmall { size_mib: u64, required_mib: u64 },

    #[error("Partition error: {0}")]
    PartitionError(String),

    #[error("Filesystem error: {0}")]
    FilesystemError(String),

    #[error("Mount error: {0}")]
    MountError(String),

    #[error("Chroot error: {0}")]
    ChrootError(String),

    #[error("Command failed: {command}\n{stderr}")]
    CommandFailed { command: String, stderr: String },

    #[error("Command not found: {0}")]
    CommandNotFound(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Validation error: {0}")]
    ValidationError(String),

    #[error("User cancelled operation")]
    UserCancelled,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

    #[error("TOML serialize error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    #[error("Nix error: {0}")]
    Nix(#[from] nix::Error),
}

pub type Result<T> = std::result::Result<T, DeploytixError>;
