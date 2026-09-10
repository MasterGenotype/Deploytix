//! This crate's own error type.
//!
//! `pkgdeps` used to raise `DeploytixError`, which meant a dependency-query
//! tool carried the installer's whole error vocabulary — partitioning
//! failures, LUKS failures, user cancellation — none of which it can produce.
//! These four are what it can actually go wrong with. The installer converts
//! at the boundary (`impl From<pkgdeps::Error> for DeploytixError`).

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    /// A tool ran but reported failure.
    #[error("command failed: {command}\n{stderr}")]
    CommandFailed { command: String, stderr: String },

    /// A required tool (`pacman`, `pactree`, `expac`) is not installed.
    #[error("required command not found: {0}")]
    CommandNotFound(String),

    /// Bad arguments, an unparseable fixture, or a package that does not exist.
    #[error("{0}")]
    Invalid(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
