//! Embedded resources and templates
//!
//! This module provides embedded configuration templates and resources
//! that are compiled into the binary for portability.

/// Runtime directory for temporary files
#[allow(dead_code)]
pub const RUNTIME_DIR: &str = "/tmp/deploytix";

/// Create runtime directory
#[allow(dead_code)]
pub fn ensure_runtime_dir() -> std::io::Result<()> {
    std::fs::create_dir_all(RUNTIME_DIR)
}

/// Clean up runtime directory
#[allow(dead_code)]
pub fn cleanup_runtime_dir() -> std::io::Result<()> {
    if std::path::Path::new(RUNTIME_DIR).exists() {
        std::fs::remove_dir_all(RUNTIME_DIR)?;
    }
    Ok(())
}

/// Write embedded resource to a path
#[allow(dead_code)]
pub fn write_resource(resource: &str, path: &str) -> std::io::Result<()> {
    std::fs::write(path, resource)
}
