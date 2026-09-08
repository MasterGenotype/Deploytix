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
        NetworkBackend::Iwd => configure_iwd(cmd, install_root, true)?,
        NetworkBackend::NetworkManager => {
            // iwd is the daemon actually driving the radio here, so it needs its
            // own configuration just as much as in the standalone case — it was
            // simply never written on this path. IP configuration stays off:
            // NetworkManager owns addressing when it fronts iwd, and both doing
            // it fights.
            configure_iwd(cmd, install_root, false)?;
            configure_nm_with_backend(cmd, install_root, "iwd")?
        }
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
        NetworkBackend::NetworkManager => {
            // Both stores, deliberately. NetworkManager and iwd are started in
            // parallel by runit/OpenRC, and an NM that comes up with
            // `wifi.backend=iwd` before iwd owns its D-Bus name leaves the Wi-Fi
            // device unmanaged for the rest of the boot — the profile sits there,
            // correct and idle, and the machine never gets online on its own.
            // Seeding iwd as well lets it associate from its own store the moment
            // it starts, whichever daemon wins the race. For a Game Mode handheld
            // that is the difference between Steam bootstrapping and a dead end
            // with no keyboard to fix it from.
            preseed_wifi_networkmanager(install_root, ssid, password)?;
            preseed_wifi_iwd(install_root, ssid, password)
        }
        NetworkBackend::NetworkManagerWpa => {
            preseed_wifi_networkmanager(install_root, ssid, password)
        }
        NetworkBackend::Iwd => preseed_wifi_iwd(install_root, ssid, password),
    }
}

/// A keyfile basename derived from `ssid`.
///
/// NetworkManager does not care what the file is called — the connection is
/// identified by `id=`/`ssid=` *inside* it — but the name still has to be a
/// single path component. SSIDs are arbitrary bytes and may legitimately
/// contain `/`, which would otherwise send the write into a directory that does
/// not exist (or, with `..`, outside the connections directory entirely).
fn nm_profile_basename(ssid: &str) -> String {
    let sanitized: String = ssid
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches(['.', ' ']);
    if trimmed.is_empty() {
        "wifi".to_string()
    } else {
        trimmed.to_string()
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
    // `autoconnect-retries=0` means retry forever. The default is 4, after
    // which NetworkManager stops autoactivating this profile until something
    // manually brings it up. First boot is exactly when those four are cheapest
    // to lose: the radio may not be up, the firmware may still be loading, or
    // the AP may not be in range yet, and four attempts can be spent in
    // seconds. The profile then sits there, correct and idle, and the machine
    // never gets online on its own — which for a Game Mode handheld means Steam
    // cannot bootstrap and there is no keyboard to fix it with.
    let profile = format!(
        "[connection]\n\
         id={ssid}\n\
         uuid={uuid}\n\
         type=wifi\n\
         autoconnect=true\n\
         autoconnect-retries=0\n\
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

    let basename = nm_profile_basename(ssid);
    let path = format!("{}/{}.nmconnection", conn_dir, basename);
    fs::write(&path, profile)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;

    info!(
        "Wi-Fi profile written to /etc/NetworkManager/system-connections/{}.nmconnection",
        basename
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

/// Configure iwd.
///
/// `owns_ip` says whether iwd is responsible for addressing. It is when iwd runs
/// standalone; it is not when NetworkManager fronts it with `wifi.backend=iwd`,
/// where NM does the addressing and having both configure the interface makes
/// them fight over it.
fn configure_iwd(cmd: &CommandRunner, install_root: &str, owns_ip: bool) -> Result<()> {
    info!("Configuring iwd (network configuration: {})", owns_ip);

    let iwd_conf_dir = format!("{}/etc/iwd", install_root);
    let iwd_conf_path = format!("{}/main.conf", iwd_conf_dir);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would configure iwd at {}", iwd_conf_path);
        return Ok(());
    }

    fs::create_dir_all(&iwd_conf_dir)?;

    let iwd_config = format!(
        "[General]\nEnableNetworkConfiguration={owns_ip}\n\n\
         [Network]\nNameResolvingService=resolvconf\nRoutePriorityOffset=300\n\
         EnableIPv6=true\n\n\
         [Scan]\nDisablePeriodicScan=false\n"
    );

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

    /// NetworkManager fronting iwd is two daemons, and only one of them was ever
    /// given the credentials. They are started in parallel by runit/OpenRC, and an
    /// NM that wins the race leaves the Wi-Fi device unmanaged for the whole boot —
    /// the pre-seeded network then never connects on startup. Seeding iwd's own
    /// store as well lets it associate from the moment it starts.
    #[test]
    fn nm_iwd_backend_preseeds_both_stores() {
        let root = tempdir("nm_iwd_both");
        let cmd = CommandRunner::new(false);
        let mut cfg = DeploymentConfig::sample();
        cfg.network.backend = NetworkBackend::NetworkManager;
        cfg.network.wifi_ssid = Some("HomeNet".to_string());
        cfg.network.wifi_password = Some("hunter2222".to_string());

        configure_network(&cmd, &cfg, root.to_str().unwrap()).unwrap();

        let nm = root.join("etc/NetworkManager/system-connections/HomeNet.nmconnection");
        let iwd = root.join("var/lib/iwd/HomeNet.psk");
        assert!(nm.exists(), "NetworkManager profile must still be written");
        assert!(
            iwd.exists(),
            "iwd must also be able to associate on its own"
        );
        assert!(std::fs::read_to_string(&iwd)
            .unwrap()
            .contains("AutoConnect=true"));
    }

    /// iwd is the daemon actually driving the radio on this path, so it needs its
    /// own configuration — which was simply never written here. Addressing stays
    /// with NetworkManager; both configuring the interface makes them fight.
    #[test]
    fn nm_iwd_backend_configures_iwd_without_ip_configuration() {
        let root = tempdir("nm_iwd_conf");
        let cmd = CommandRunner::new(false);
        let mut cfg = DeploymentConfig::sample();
        cfg.network.backend = NetworkBackend::NetworkManager;

        configure_network(&cmd, &cfg, root.to_str().unwrap()).unwrap();

        let conf = std::fs::read_to_string(root.join("etc/iwd/main.conf"))
            .expect("iwd must be configured when it is NM's backend");
        assert!(conf.contains("EnableNetworkConfiguration=false"));
        let nm_conf =
            std::fs::read_to_string(root.join("etc/NetworkManager/conf.d/wifi-backend.conf"))
                .unwrap();
        assert!(nm_conf.contains("wifi.backend=iwd"));
    }

    /// Standalone iwd has no NetworkManager above it, so it must do addressing.
    #[test]
    fn standalone_iwd_owns_ip_configuration() {
        let root = tempdir("iwd_standalone");
        let cmd = CommandRunner::new(false);
        let mut cfg = DeploymentConfig::sample();
        cfg.network.backend = NetworkBackend::Iwd;

        configure_network(&cmd, &cfg, root.to_str().unwrap()).unwrap();

        let conf = std::fs::read_to_string(root.join("etc/iwd/main.conf")).unwrap();
        assert!(conf.contains("EnableNetworkConfiguration=true"));
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

    /// `autoconnect=true` alone is not enough to keep a machine trying.
    /// NetworkManager gives up after `autoconnect-retries` (default 4) failed
    /// activations and will not autoactivate the profile again until something
    /// brings it up by hand — and on a Game Mode handheld there is nothing to
    /// do that with. First boot is where those four attempts are most likely to
    /// be burned on a radio or AP that is not ready yet.
    #[test]
    fn nm_preseed_retries_forever() {
        let root = tempdir("nm_retries");
        preseed_wifi_networkmanager(root.to_str().unwrap(), "HomeNet", Some("hunter2222")).unwrap();

        let path = root.join("etc/NetworkManager/system-connections/HomeNet.nmconnection");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("autoconnect=true"));
        assert!(
            content.contains("autoconnect-retries=0"),
            "0 means retry forever; the default of 4 strands the machine offline"
        );
        // Both keys belong to [connection], before the first other section.
        let connection = &content[..content.find("[wifi]").expect("has a [wifi] section")];
        assert!(connection.contains("autoconnect=true"));
        assert!(connection.contains("autoconnect-retries=0"));
    }

    #[test]
    fn iwd_preseed_autoconnects() {
        let root = tempdir("iwd_auto");
        preseed_wifi_iwd(root.to_str().unwrap(), "HomeNet", Some("hunter2222")).unwrap();
        let content = std::fs::read_to_string(root.join("var/lib/iwd/HomeNet.psk")).unwrap();
        assert!(content.contains("[Settings]"));
        assert!(content.contains("AutoConnect=true"));

        // Open networks too — the settings block is easy to lose when there is
        // no [Security] group above it.
        let root = tempdir("iwd_auto_open");
        preseed_wifi_iwd(root.to_str().unwrap(), "CafeWifi", None).unwrap();
        let content = std::fs::read_to_string(root.join("var/lib/iwd/CafeWifi.open")).unwrap();
        assert!(content.contains("AutoConnect=true"));
    }

    /// SSIDs are arbitrary bytes. A `/` in one used to be spliced straight into
    /// the path, so the write landed in a directory that does not exist — or,
    /// with `..`, outside the connections directory.
    #[test]
    fn nm_profile_basename_is_a_single_safe_path_component() {
        assert_eq!(nm_profile_basename("HomeNet"), "HomeNet");
        assert_eq!(nm_profile_basename("Home Net-2.4"), "Home Net-2.4");
        assert_eq!(nm_profile_basename("Guest/Wifi"), "Guest_Wifi");
        // Path separators become underscores and the leading dots are
        // trimmed; the point is that the result is one harmless component,
        // not that it is pretty.
        assert_eq!(nm_profile_basename("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(nm_profile_basename("Café!"), "Caf__");
        // Never empty, never a bare dot entry.
        assert_eq!(nm_profile_basename(""), "wifi");
        assert_eq!(nm_profile_basename("."), "wifi");
        assert_eq!(nm_profile_basename(".."), "wifi");
        for ssid in ["Guest/Wifi", "../../etc/passwd", "", ".", ".."] {
            let name = nm_profile_basename(ssid);
            assert!(
                !name.contains('/'),
                "{ssid:?} -> {name:?} is not one component"
            );
            assert!(name != "." && name != "..", "{ssid:?} -> {name:?}");
        }
    }

    /// The file name is sanitized, but the profile must still carry the real
    /// SSID — that is what NetworkManager actually matches the network on.
    #[test]
    fn nm_preseed_keeps_the_true_ssid_when_the_filename_is_sanitized() {
        let root = tempdir("nm_slash");
        preseed_wifi_networkmanager(root.to_str().unwrap(), "Guest/Wifi", Some("pw")).unwrap();

        let path = root.join("etc/NetworkManager/system-connections/Guest_Wifi.nmconnection");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("ssid=Guest/Wifi"),
            "the real SSID must survive"
        );
        assert!(content.contains("id=Guest/Wifi"));
        assert!(content.contains("autoconnect-retries=0"));
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
