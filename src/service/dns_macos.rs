//! macOS DNS surgery via `networksetup`.
//!
//! Why this lives here (not in the GUI): in China the TUN-mode user has to
//! point system DNS at the TUN gateway (198.18.0.1) or the LAN router's
//! upstream DNS poisons foreign domain lookups. This requires admin elevation
//! per `networksetup -setdnsservers`. Pre-helper, the Dart side popped
//! osascript every connect — annoying. Helper makes it one IPC, no dialog.
//!
//! Operations needed by the GUI:
//!   * `SetDns(servers)` — for **each enabled network service**, set DNS to
//!     `servers`. List comes from `networksetup -listallnetworkservices`.
//!   * `ClearDns` — for each service, `networksetup -setdnsservers <svc> empty`
//!     to revert to DHCP. The string "empty" (lowercase) is the official
//!     sentinel per `man networksetup`.
//!
//! After either, we `dscacheutil -flushcache; killall -HUP mDNSResponder` so
//! the in-kernel resolver cache picks up the change. Without this the OS
//! keeps serving cached records for ~10 min, and "DNS changed but YouTube
//! still doesn't load" is a confusing user experience.

#![cfg(target_os = "macos")]

use anyhow::{Context, Result};
use tokio::process::Command;

/// Run `networksetup -listallnetworkservices` and parse out the names.
///
/// The first line is a human-readable preamble ("An asterisk (*) denotes
/// that a network service is disabled."). Disabled services have a `*`
/// prefix. We drop both.
///
/// Returns service names like ["Wi-Fi", "USB 10/100/1000 LAN", ...] —
/// these are exact strings to pass back into other `networksetup` calls.
async fn list_services() -> Result<Vec<String>> {
    let out = Command::new("networksetup")
        .arg("-listallnetworkservices")
        .output()
        .await
        .context("spawning networksetup -listallnetworkservices")?;
    if !out.status.success() {
        anyhow::bail!(
            "networksetup -listallnetworkservices failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut services = Vec::new();
    for (i, line) in stdout.lines().enumerate() {
        // Line 0 is the preamble.
        if i == 0 {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Disabled services start with `*` — skip; setting DNS on them is
        // a no-op anyway and just wastes a fork.
        if trimmed.starts_with('*') {
            continue;
        }
        services.push(trimmed.to_string());
    }
    Ok(services)
}

/// Set DNS for every enabled service to `servers`. Empty list is rejected —
/// caller meant `clear()` instead, and empty here would silently leave the
/// previous DNS in place (networksetup's behavior).
pub async fn set_dns(servers: &[String]) -> Result<()> {
    if servers.is_empty() {
        anyhow::bail!("set_dns called with empty servers; use clear() to revert to DHCP");
    }
    let services = list_services().await?;
    let mut errors = Vec::new();
    for svc in &services {
        let mut cmd = Command::new("networksetup");
        cmd.arg("-setdnsservers").arg(svc);
        for s in servers {
            cmd.arg(s);
        }
        match cmd.output().await {
            Ok(out) if out.status.success() => {
                log::info!("set DNS on {:?} to {:?}", svc, servers);
            }
            Ok(out) => errors.push(format!(
                "setdnsservers {:?}: {}",
                svc,
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            Err(e) => errors.push(format!("setdnsservers {:?}: spawn failed: {e}", svc)),
        }
    }
    flush_cache().await?;
    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("setdnsservers partial failure: {}", errors.join("; "))
    }
}

/// Revert every enabled service back to DHCP-provided DNS.
///
/// `networksetup -setdnsservers <svc> empty` is the documented way to do
/// this. `empty` is a literal keyword, not a placeholder.
pub async fn clear_dns() -> Result<()> {
    let services = list_services().await?;
    let mut errors = Vec::new();
    for svc in &services {
        match Command::new("networksetup")
            .args(["-setdnsservers", svc.as_str(), "empty"])
            .output()
            .await
        {
            Ok(out) if out.status.success() => {
                log::info!("cleared DNS on {:?}", svc);
            }
            Ok(out) => errors.push(format!(
                "setdnsservers empty {:?}: {}",
                svc,
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            Err(e) => errors.push(format!("setdnsservers empty {:?}: spawn failed: {e}", svc)),
        }
    }
    flush_cache().await?;
    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("clear_dns partial failure: {}", errors.join("; "))
    }
}

/// `dscacheutil -flushcache; killall -HUP mDNSResponder`. Both are required
/// because flushcache only touches the user-space DirectoryService cache and
/// HUP-mDNSResponder triggers the kernel-side mDNSResponder to reload its
/// own config + clear its in-process cache. Skipping HUP means recent CN
/// domain lookups stick to the old DNS for up to 10 minutes.
async fn flush_cache() -> Result<()> {
    let _ = Command::new("dscacheutil")
        .arg("-flushcache")
        .status()
        .await
        .context("dscacheutil -flushcache")?;
    let _ = Command::new("killall")
        .args(["-HUP", "mDNSResponder"])
        .status()
        .await
        .context("killall -HUP mDNSResponder")?;
    Ok(())
}
