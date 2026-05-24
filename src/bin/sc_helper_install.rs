//! `sc-helper-install` — runs ONCE under admin to wire the helper into
//! launchd / SCM and exit. The GUI spawns this via `osascript ... with
//! administrator privileges` (mac) or via UAC manifest (Win, phase 3).
//!
//! What it does (mac):
//!   1. Copy `sc-helper` (sibling of this binary) into
//!      `/Library/PrivilegedHelperTools/com.scloud.helper/sc-helper`.
//!   2. Copy `sc-mihomo` (also sibling) next to it. The helper expects to
//!      find mihomo as a sibling — see `resolve_mihomo_path` in main.rs.
//!   3. Write `/Library/LaunchDaemons/com.scloud.helper.plist`.
//!   4. chown root:wheel + chmod 644 plist; chmod 755 binaries.
//!   5. `launchctl bootstrap system /Library/LaunchDaemons/com.scloud.helper.plist`.
//!   6. Exit 0. Helper is now running.
//!
//! Idempotent: re-running upgrades (bootout, replace files, bootstrap again).

use anyhow::{bail, Result};
#[cfg(target_os = "macos")]
use anyhow::{anyhow, Context};
#[cfg(target_os = "macos")]
use sc_helper::paths;
#[cfg(target_os = "macos")]
use std::path::Path;

#[cfg(target_os = "macos")]
fn main() -> Result<()> {
    eprintln!("[sc-helper-install] starting");

    // Where are we? Sibling binaries should be `sc-helper` + `sc-mihomo`.
    let me = std::env::current_exe().context("locating own binary")?;
    let payload_dir = me
        .parent()
        .ok_or_else(|| anyhow!("current exe has no parent dir"))?;

    let src_helper = payload_dir.join("sc-helper");
    let src_mihomo = payload_dir.join("sc-mihomo");

    if !src_helper.is_file() {
        bail!(
            "expected sibling binary sc-helper at {}",
            src_helper.display()
        );
    }
    // sc-mihomo is optional during phase 0 dev — we may not have it bundled
    // yet. Warn but proceed; the helper itself will refuse to start mihomo
    // until the binary appears.
    if !src_mihomo.is_file() {
        eprintln!(
            "[sc-helper-install] WARN: sc-mihomo missing at {}; helper will start but StartMihomo will fail until it's added",
            src_mihomo.display()
        );
    }

    // 1. Make install dir.
    let install_dir = Path::new(paths::macos::HELPER_INSTALL_DIR);
    std::fs::create_dir_all(install_dir)
        .with_context(|| format!("mkdir -p {}", install_dir.display()))?;

    // 2. Copy binaries.
    let dst_helper = install_dir.join("sc-helper");
    let dst_mihomo = install_dir.join("sc-mihomo");

    // If helper is currently running, bootout first so we can overwrite the
    // binary. launchd holds an open handle to a running binary and on macOS
    // you can replace it (unlike Win) but cleaner to stop, copy, restart.
    if Path::new(paths::macos::LAUNCHD_PLIST).is_file() {
        eprintln!("[sc-helper-install] existing install detected — bootout first");
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &format!("system/{}", paths::macos::LAUNCHD_LABEL)])
            .status();
    }

    std::fs::copy(&src_helper, &dst_helper)
        .with_context(|| format!("copy {} → {}", src_helper.display(), dst_helper.display()))?;
    eprintln!("[sc-helper-install] copied sc-helper");

    if src_mihomo.is_file() {
        std::fs::copy(&src_mihomo, &dst_mihomo).with_context(|| {
            format!("copy {} → {}", src_mihomo.display(), dst_mihomo.display())
        })?;
        eprintln!("[sc-helper-install] copied sc-mihomo");
    }

    // 3. Write plist. Template lives in files/, baked in at compile time.
    let plist_body = render_plist();
    std::fs::write(paths::macos::LAUNCHD_PLIST, &plist_body).with_context(|| {
        format!("writing {}", paths::macos::LAUNCHD_PLIST)
    })?;
    eprintln!("[sc-helper-install] wrote plist");

    // 4. Set ownership + perms.
    chown_root_wheel(&dst_helper)?;
    chmod(&dst_helper, 0o755)?;
    if dst_mihomo.is_file() {
        chown_root_wheel(&dst_mihomo)?;
        chmod(&dst_mihomo, 0o755)?;
    }
    chown_root_wheel(Path::new(paths::macos::LAUNCHD_PLIST))?;
    chmod(Path::new(paths::macos::LAUNCHD_PLIST), 0o644)?;
    eprintln!("[sc-helper-install] perms set");

    // 5. Bootstrap.
    let status = std::process::Command::new("launchctl")
        .args(["bootstrap", "system", paths::macos::LAUNCHD_PLIST])
        .status()
        .context("spawning launchctl")?;
    if !status.success() {
        // bootstrap also fails if the daemon was already loaded; we did our
        // best with bootout above but `not enabled` etc. is recoverable.
        // Treat non-zero as warning, not fatal.
        eprintln!("[sc-helper-install] WARN: launchctl bootstrap exited {}", status);
    } else {
        eprintln!("[sc-helper-install] launchctl bootstrap OK");
    }

    eprintln!("[sc-helper-install] done");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() -> Result<()> {
    bail!("sc-helper-install: this binary is macOS-only in phase 0. Windows installer comes in phase 3.")
}

#[cfg(target_os = "macos")]
fn render_plist() -> String {
    // Inline the template (kept in sync with files/com.scloud.helper.plist.tmpl
    // by hand for now — once it stabilizes we'll include_str! it).
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{stdout}</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>RUST_LOG</key>
        <string>info</string>
    </dict>
</dict>
</plist>
"#,
        label = paths::macos::LAUNCHD_LABEL,
        bin = paths::macos::HELPER_BINARY,
        stdout = paths::macos::STDOUT_LOG,
        stderr = paths::macos::STDERR_LOG,
    )
}

#[cfg(target_os = "macos")]
fn chown_root_wheel(path: &Path) -> Result<()> {
    // root = uid 0, wheel = gid 0 on macOS.
    use nix::unistd::{chown, Gid, Uid};
    chown(path, Some(Uid::from_raw(0)), Some(Gid::from_raw(0)))
        .with_context(|| format!("chown root:wheel {}", path.display()))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn chmod(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .permissions();
    perms.set_mode(mode);
    std::fs::set_permissions(path, perms)
        .with_context(|| format!("chmod {:o} {}", mode, path.display()))?;
    Ok(())
}

