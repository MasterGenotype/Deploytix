//! Embedded resources and templates
//!
//! This module provides embedded configuration templates and resources
//! that are compiled into the binary for portability.

/// Embedded dnscrypt-proxy configuration template
pub const DNSCRYPT_PROXY_TOML: &str = include_str!("dnscrypt-proxy.toml");

/// Runtime directory for temporary files
pub const RUNTIME_DIR: &str = "/tmp/deploytix";

/// Create runtime directory
pub fn ensure_runtime_dir() -> std::io::Result<()> {
    std::fs::create_dir_all(RUNTIME_DIR)
}

/// Clean up runtime directory
pub fn cleanup_runtime_dir() -> std::io::Result<()> {
    if std::path::Path::new(RUNTIME_DIR).exists() {
        std::fs::remove_dir_all(RUNTIME_DIR)?;
    }
    Ok(())
}

/// Write embedded resource to a path
pub fn write_resource(resource: &str, path: &str) -> std::io::Result<()> {
    std::fs::write(path, resource)
}
