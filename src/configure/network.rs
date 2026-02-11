//! Network configuration

use crate::config::{DeploymentConfig, DnsProvider, NetworkBackend};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use tracing::info;

/// Configure network settings
pub fn configure_network(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    info!("Configuring network (backend: {}, dns: {})", config.network.backend, config.network.dns);

    // Configure network backend
    match config.network.backend {
        NetworkBackend::Iwd => configure_iwd(cmd, install_root)?,
        NetworkBackend::NetworkManager => configure_networkmanager(cmd, install_root)?,
        NetworkBackend::Connman => configure_connman(cmd, install_root)?,
    }

    // Configure DNS
    match config.network.dns {
        DnsProvider::DnscryptProxy => configure_dnscrypt(cmd, install_root)?,
        DnsProvider::Systemd => {} // Not applicable on Artix
        DnsProvider::None => {}
    }

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

/// Configure NetworkManager to use iwd backend
fn configure_networkmanager(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("Configuring NetworkManager with iwd backend");

    let nm_conf_dir = format!("{}/etc/NetworkManager/conf.d", install_root);
    let nm_conf_path = format!("{}/iwd.conf", nm_conf_dir);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would configure NetworkManager at {}", nm_conf_path);
        return Ok(());
    }

    fs::create_dir_all(&nm_conf_dir)?;

    let nm_config = r#"[device]
wifi.backend=iwd
"#;

    fs::write(&nm_conf_path, nm_config)?;

    info!("NetworkManager configuration written");
    Ok(())
}

/// Configure ConnMan
fn configure_connman(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("Configuring ConnMan");

    let connman_conf_path = format!("{}/etc/connman/main.conf", install_root);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would configure ConnMan at {}", connman_conf_path);
        return Ok(());
    }

    fs::create_dir_all(format!("{}/etc/connman", install_root))?;

    let connman_config = r#"[General]
PreferredTechnologies=ethernet,wifi
SingleConnectedTechnology=false
AllowHostnameUpdates=false
PersistentTetheringMode=true
"#;

    fs::write(&connman_conf_path, connman_config)?;

    info!("ConnMan configuration written");
    Ok(())
}

/// Configure dnscrypt-proxy
fn configure_dnscrypt(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("Configuring dnscrypt-proxy");

    let dnscrypt_conf_dir = format!("{}/etc/dnscrypt-proxy", install_root);
    let dnscrypt_conf_path = format!("{}/dnscrypt-proxy.toml", dnscrypt_conf_dir);
    let resolvconf_path = format!("{}/etc/resolvconf.conf", install_root);
    let resolv_path = format!("{}/etc/resolv.conf", install_root);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would configure dnscrypt-proxy");
        return Ok(());
    }

    fs::create_dir_all(&dnscrypt_conf_dir)?;

    // Basic dnscrypt-proxy config
    let dnscrypt_config = r#"# dnscrypt-proxy configuration
listen_addresses = ['127.0.0.1:53', '[::1]:53']
server_names = ['cloudflare', 'cloudflare-ipv6', 'google', 'google-ipv6']
ipv4_servers = true
ipv6_servers = true
dnscrypt_servers = true
doh_servers = true
require_dnssec = false
require_nolog = true
require_nofilter = true
force_tcp = false
timeout = 5000
keepalive = 30
log_level = 2
use_syslog = true
cert_refresh_delay = 240
fallback_resolvers = ['1.1.1.1:53', '8.8.8.8:53']
ignore_system_dns = true
netprobe_timeout = 60
netprobe_address = '1.1.1.1:53'
block_ipv6 = false
block_unqualified = true
block_undelegated = true
reject_ttl = 600

[sources]
  [sources.'public-resolvers']
  urls = ['https://raw.githubusercontent.com/DNSCrypt/dnscrypt-resolvers/master/v3/public-resolvers.md']
  cache_file = '/var/cache/dnscrypt-proxy/public-resolvers.md'
  minisign_key = 'RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3'
  refresh_delay = 72
"#;

    fs::write(&dnscrypt_conf_path, dnscrypt_config)?;

    // Configure resolvconf
    let resolvconf_config = "name_servers=127.0.0.1\n";
    fs::write(&resolvconf_path, resolvconf_config)?;

    // Set resolv.conf
    let resolv_config = "nameserver 127.0.0.1\n";
    // Remove symlink if exists
    let _ = fs::remove_file(&resolv_path);
    fs::write(&resolv_path, resolv_config)?;

    info!("dnscrypt-proxy configuration written");
    Ok(())
}
