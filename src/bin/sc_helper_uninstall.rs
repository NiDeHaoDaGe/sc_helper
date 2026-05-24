//! `sc-helper-uninstall` — runs ONCE under admin to remove the daemon.
//! Symmetric counterpart of `sc-helper-install`.
//!
//! Mac steps:
//!   1. Best-effort `launchctl bootout system/<label>`.
//!   2. rm plist.
//!   3. rm install dir (sc-helper + sc-mihomo).
//!   4. rm socket if still present.
//!
//! All steps are best-effort — a failed bootout (e.g. helper already exited)
//! shouldn't stop us from cleaning up files. We accumulate errors and print
//! them at the end, but exit 0 unless something catastrophic happened.

use anyhow::{bail, Result};
#[cfg(target_os = "macos")]
use sc_helper::paths;

#[cfg(target_os = "macos")]
fn main() -> Result<()> {
    eprintln!("[sc-helper-uninstall] starting");

    let mut errors: Vec<String> = Vec::new();

    // 1. bootout.
    let target = format!("system/{}", paths::macos::LAUNCHD_LABEL);
    let status = std::process::Command::new("launchctl")
        .args(["bootout", &target])
        .status();
    match status {
        Ok(s) if s.success() => eprintln!("[sc-helper-uninstall] bootout OK"),
        Ok(s) => eprintln!("[sc-helper-uninstall] bootout exited {} (likely already stopped)", s),
        Err(e) => errors.push(format!("bootout: {e}")),
    }

    // 2. rm plist.
    if std::path::Path::new(paths::macos::LAUNCHD_PLIST).exists() {
        if let Err(e) = std::fs::remove_file(paths::macos::LAUNCHD_PLIST) {
            errors.push(format!("rm plist: {e}"));
        } else {
            eprintln!("[sc-helper-uninstall] removed plist");
        }
    }

    // 3. rm install dir.
    if std::path::Path::new(paths::macos::HELPER_INSTALL_DIR).exists() {
        if let Err(e) = std::fs::remove_dir_all(paths::macos::HELPER_INSTALL_DIR) {
            errors.push(format!("rm install dir: {e}"));
        } else {
            eprintln!("[sc-helper-uninstall] removed install dir");
        }
    }

    // 4. rm socket (helper would clean this on graceful shutdown, but if it
    // crashed earlier the socket file lingers).
    if std::path::Path::new(paths::macos::SOCKET_PATH).exists() {
        let _ = std::fs::remove_file(paths::macos::SOCKET_PATH);
    }

    if errors.is_empty() {
        eprintln!("[sc-helper-uninstall] done");
        Ok(())
    } else {
        // Non-fatal but worth logging.
        for e in &errors {
            eprintln!("[sc-helper-uninstall] WARN: {e}");
        }
        eprintln!(
            "[sc-helper-uninstall] done with {} warning(s)",
            errors.len()
        );
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
fn main() -> Result<()> {
    bail!("sc-helper-uninstall: this binary is macOS-only in phase 0. Windows uninstaller comes in phase 3.")
}
