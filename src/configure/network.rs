//! Network configuration

use crate::config::{DeploymentConfig, NetworkBackend};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tracing::info;

/// Configure network settings
pub fn configure_network(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    info!("Configuring network (backend: {})", config.network.backend);

    // Configure network backend
    match config.network.backend {
        NetworkBackend::Iwd => configure_iwd(cmd, install_root)?,
        NetworkBackend::NetworkManager => configure_nm_with_backend(cmd, install_root, "iwd")?,
        NetworkBackend::NetworkManagerWpa => {
            configure_nm_with_backend(cmd, install_root, "wpa_supplicant")?
        }
    }

    // Pre-seed a Wi-Fi network so the system has connectivity from the very
    // first boot (Steam's first-run client bootstrap in the gamescope session
    // needs network before its own OOBE network page exists).
    if let Some(ssid) = &config.network.wifi_ssid {
        preseed_wifi(
            cmd,
            config,
            install_root,
            ssid,
            config.network.wifi_password.as_deref(),
        )?;
    }

    Ok(())
}

/// Write credentials for one Wi-Fi network to the target system so it
/// auto-connects on first boot.
///
/// - NetworkManager backends: a keyfile connection profile in
///   `/etc/NetworkManager/system-connections/<ssid>.nmconnection` (mode 0600 —
///   NetworkManager refuses profiles readable by others).
/// - Standalone iwd backend: a network file in `/var/lib/iwd/` named after
///   the SSID (`<ssid>.psk` / `<ssid>.open`), hex-encoded per iwd convention
///   when the SSID contains characters outside `[A-Za-z0-9_- ]`.
fn preseed_wifi(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
    ssid: &str,
    password: Option<&str>,
) -> Result<()> {
    info!("Pre-seeding Wi-Fi network '{}'", ssid);

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would pre-seed Wi-Fi network '{}' ({}, backend: {})",
            ssid,
            if password.is_some() {
                "WPA-PSK"
            } else {
                "open"
            },
            config.network.backend
        );
        return Ok(());
    }

    match config.network.backend {
        NetworkBackend::NetworkManager | NetworkBackend::NetworkManagerWpa => {
            preseed_wifi_networkmanager(install_root, ssid, password)
        }
        NetworkBackend::Iwd => preseed_wifi_iwd(install_root, ssid, password),
    }
}

fn preseed_wifi_networkmanager(
    install_root: &str,
    ssid: &str,
    password: Option<&str>,
) -> Result<()> {
    let conn_dir = format!("{}/etc/NetworkManager/system-connections", install_root);
    fs::create_dir_all(&conn_dir)?;

    let uuid = uuid::Uuid::new_v4();

    let security = match password {
        Some(psk) => format!("\n[wifi-security]\nkey-mgmt=wpa-psk\npsk={}\n", psk),
        None => String::new(),
    };
    let profile = format!(
        "[connection]\n\
         id={ssid}\n\
         uuid={uuid}\n\
         type=wifi\n\
         autoconnect=true\n\
         \n\
         [wifi]\n\
         mode=infrastructure\n\
         ssid={ssid}\n\
         {security}\n\
         [ipv4]\n\
         method=auto\n\
         \n\
         [ipv6]\n\
         method=auto\n"
    );

    let path = format!("{}/{}.nmconnection", conn_dir, ssid);
    fs::write(&path, profile)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;

    info!(
        "Wi-Fi profile written to /etc/NetworkManager/system-connections/{}.nmconnection",
        ssid
    );
    Ok(())
}

fn preseed_wifi_iwd(install_root: &str, ssid: &str, password: Option<&str>) -> Result<()> {
    let iwd_dir = format!("{}/var/lib/iwd", install_root);
    fs::create_dir_all(&iwd_dir)?;
    fs::set_permissions(&iwd_dir, fs::Permissions::from_mode(0o700))?;

    // iwd names network files after the SSID directly when it only contains
    // alphanumerics, '-', '_' and ' '; otherwise `=` followed by the
    // hex-encoded SSID bytes.
    let simple = ssid
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ' '));
    let file_stem = if simple {
        ssid.to_string()
    } else {
        let hex: String = ssid.bytes().map(|b| format!("{:02x}", b)).collect();
        format!("={}", hex)
    };

    let (extension, content) = match password {
        Some(psk) => (
            "psk",
            format!(
                "[Security]\nPassphrase={}\n\n[Settings]\nAutoConnect=true\n",
                psk
            ),
        ),
        None => ("open", "[Settings]\nAutoConnect=true\n".to_string()),
    };

    let path = format!("{}/{}.{}", iwd_dir, file_stem, extension);
    fs::write(&path, content)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;

    info!(
        "Wi-Fi network file written to /var/lib/iwd/{}.{}",
        file_stem, extension
    );
    Ok(())
}

/// Configure iwd
fn configure_iwd(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("Configuring iwd");

    let iwd_conf_dir = format!("{}/etc/iwd", install_root);
    let iwd_conf_path = format!("{}/main.conf", iwd_conf_dir);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would configure iwd at {}", iwd_conf_path);
        return Ok(());
    }

    fs::create_dir_all(&iwd_conf_dir)?;

    let iwd_config = r#"[General]
EnableNetworkConfiguration=true

[Network]
NameResolvingService=resolvconf
RoutePriorityOffset=300
EnableIPv6=true

[Scan]
DisablePeriodicScan=false
"#;

    fs::write(&iwd_conf_path, iwd_config)?;

    info!("iwd configuration written");
    Ok(())
}

/// Configure NetworkManager with the given wifi backend ("iwd" or "wpa_supplicant").
fn configure_nm_with_backend(
    cmd: &CommandRunner,
    install_root: &str,
    wifi_backend: &str,
) -> Result<()> {
    info!("Configuring NetworkManager with {} backend", wifi_backend);

    let nm_conf_dir = format!("{}/etc/NetworkManager/conf.d", install_root);
    let nm_conf_path = format!("{}/wifi-backend.conf", nm_conf_dir);

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would configure NetworkManager at {} (wifi.backend={})",
            nm_conf_path, wifi_backend
        );
        return Ok(());
    }

    fs::create_dir_all(&nm_conf_dir)?;

    let nm_config = format!("[device]\nwifi.backend={}\n", wifi_backend);
    fs::write(&nm_conf_path, nm_config)?;

    info!("NetworkManager configuration written");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "deploytix_network_test_{}_{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn nm_preseed_writes_psk_profile_with_0600() {
        let root = tempdir("nm_psk");
        preseed_wifi_networkmanager(root.to_str().unwrap(), "HomeNet", Some("hunter2222")).unwrap();

        let path = root.join("etc/NetworkManager/system-connections/HomeNet.nmconnection");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("id=HomeNet"));
        assert!(content.contains("ssid=HomeNet"));
        assert!(content.contains("key-mgmt=wpa-psk"));
        assert!(content.contains("psk=hunter2222"));
        assert!(content.contains("autoconnect=true"));

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "NM refuses profiles readable by others"
        );
    }

    #[test]
    fn nm_preseed_open_network_has_no_security_section() {
        let root = tempdir("nm_open");
        preseed_wifi_networkmanager(root.to_str().unwrap(), "CafeWifi", None).unwrap();

        let path = root.join("etc/NetworkManager/system-connections/CafeWifi.nmconnection");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("[wifi-security]"));
        assert!(content.contains("ssid=CafeWifi"));
    }

    #[test]
    fn iwd_preseed_uses_plain_name_for_simple_ssid() {
        let root = tempdir("iwd_plain");
        preseed_wifi_iwd(root.to_str().unwrap(), "Home Net-2", Some("hunter2222")).unwrap();

        let path = root.join("var/lib/iwd/Home Net-2.psk");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Passphrase=hunter2222"));
        assert!(content.contains("AutoConnect=true"));
    }

    #[test]
    fn iwd_preseed_hex_encodes_special_ssid() {
        let root = tempdir("iwd_hex");
        preseed_wifi_iwd(root.to_str().unwrap(), "Café!", None).unwrap();

        // "Café!" UTF-8 bytes: 43 61 66 c3 a9 21 — open network → .open file
        let path = root.join("var/lib/iwd/=436166c3a921.open");
        assert!(path.exists(), "expected hex-encoded iwd filename");
    }
}
