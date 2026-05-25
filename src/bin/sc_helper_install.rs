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
#[cfg(any(target_os = "macos", target_os = "linux"))]
use anyhow::{anyhow, Context};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use sc_helper::paths;
#[cfg(any(target_os = "macos", target_os = "linux"))]
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
    // ping tool — useful for support diagnostics, no privilege needed at
    // runtime but we ship it alongside helper so the path is predictable.
    let src_ping = payload_dir.join("sc-helper-ping");

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
    if !src_ping.is_file() {
        eprintln!(
            "[sc-helper-install] WARN: sc-helper-ping missing at {}; support diagnostics will be unavailable",
            src_ping.display()
        );
    }

    // 1. Make install dir.
    let install_dir = Path::new(paths::macos::HELPER_INSTALL_DIR);
    std::fs::create_dir_all(install_dir)
        .with_context(|| format!("mkdir -p {}", install_dir.display()))?;

    // 2. Copy binaries.
    let dst_helper = install_dir.join("sc-helper");
    let dst_mihomo = install_dir.join("sc-mihomo");
    let dst_ping = install_dir.join("sc-helper-ping");

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

    if src_ping.is_file() {
        std::fs::copy(&src_ping, &dst_ping).with_context(|| {
            format!("copy {} → {}", src_ping.display(), dst_ping.display())
        })?;
        eprintln!("[sc-helper-install] copied sc-helper-ping");
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
    if dst_ping.is_file() {
        // ping tool stays root-owned (we put it under /Library) but world-
        // executable so any user can run it for a support diagnostic.
        chown_root_wheel(&dst_ping)?;
        chmod(&dst_ping, 0o755)?;
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

#[cfg(target_os = "linux")]
fn main() -> Result<()> {
    eprintln!("[sc-helper-install] starting (linux/systemd)");

    let me = std::env::current_exe().context("locating own binary")?;
    let payload_dir = me
        .parent()
        .ok_or_else(|| anyhow!("current exe has no parent dir"))?;

    let src_helper = payload_dir.join("sc-helper");
    let src_mihomo = payload_dir.join("sc-mihomo");
    let src_ping = payload_dir.join("sc-helper-ping");

    if !src_helper.is_file() {
        bail!(
            "expected sibling binary sc-helper at {}",
            src_helper.display()
        );
    }
    // 跟 mac 一样 mihomo + ping 是 optional during dev — warn but proceed.
    if !src_mihomo.is_file() {
        eprintln!(
            "[sc-helper-install] WARN: sc-mihomo missing at {}; helper will start but StartMihomo will fail until it's added",
            src_mihomo.display()
        );
    }
    if !src_ping.is_file() {
        eprintln!(
            "[sc-helper-install] WARN: sc-helper-ping missing at {}; support diagnostics will be unavailable",
            src_ping.display()
        );
    }

    // 1. 已有 service 先 stop + disable, 这样 binary 能覆盖. systemctl
    // 操作返非零不致命 (service 可能本来就没装).
    if Path::new(paths::linux::SYSTEMD_UNIT_PATH).is_file() {
        eprintln!("[sc-helper-install] existing install detected — stop + disable first");
        let _ = std::process::Command::new("systemctl")
            .args(["disable", "--now", paths::linux::SERVICE_NAME])
            .status();
    }

    // 2. 创建 install dir + copy binaries.
    let install_dir = Path::new(paths::linux::HELPER_INSTALL_DIR);
    std::fs::create_dir_all(install_dir)
        .with_context(|| format!("mkdir -p {}", install_dir.display()))?;

    let dst_helper = install_dir.join("sc-helper");
    let dst_mihomo = install_dir.join("sc-mihomo");
    let dst_ping = install_dir.join("sc-helper-ping");

    std::fs::copy(&src_helper, &dst_helper)
        .with_context(|| format!("copy {} → {}", src_helper.display(), dst_helper.display()))?;
    eprintln!("[sc-helper-install] copied sc-helper");

    if src_mihomo.is_file() {
        std::fs::copy(&src_mihomo, &dst_mihomo).with_context(|| {
            format!("copy {} → {}", src_mihomo.display(), dst_mihomo.display())
        })?;
        eprintln!("[sc-helper-install] copied sc-mihomo");
    }

    if src_ping.is_file() {
        std::fs::copy(&src_ping, &dst_ping).with_context(|| {
            format!("copy {} → {}", src_ping.display(), dst_ping.display())
        })?;
        eprintln!("[sc-helper-install] copied sc-helper-ping");
    }

    // 3. 写 systemd unit. include_str! 编译期把模板包进 binary, 不依赖
    // runtime 文件 lookup.
    let unit_body = include_str!("../../files/sc-helper.service.tmpl");
    std::fs::write(paths::linux::SYSTEMD_UNIT_PATH, unit_body).with_context(|| {
        format!("writing {}", paths::linux::SYSTEMD_UNIT_PATH)
    })?;
    eprintln!("[sc-helper-install] wrote systemd unit");

    // 4. perms: Linux root:root (mac wheel, Linux 普通 distro 是 root).
    chown_root(&dst_helper)?;
    chmod(&dst_helper, 0o755)?;
    if dst_mihomo.is_file() {
        chown_root(&dst_mihomo)?;
        chmod(&dst_mihomo, 0o755)?;
    }
    if dst_ping.is_file() {
        chown_root(&dst_ping)?;
        chmod(&dst_ping, 0o755)?;
    }
    chown_root(Path::new(paths::linux::SYSTEMD_UNIT_PATH))?;
    chmod(Path::new(paths::linux::SYSTEMD_UNIT_PATH), 0o644)?;
    eprintln!("[sc-helper-install] perms set");

    // 5. systemctl daemon-reload + enable + start. 三步合一个 enable --now.
    let status = std::process::Command::new("systemctl")
        .arg("daemon-reload")
        .status()
        .context("spawning systemctl daemon-reload")?;
    if !status.success() {
        eprintln!("[sc-helper-install] WARN: daemon-reload exited {}", status);
    }
    let status = std::process::Command::new("systemctl")
        .args(["enable", "--now", paths::linux::SERVICE_NAME])
        .status()
        .context("spawning systemctl enable --now")?;
    if !status.success() {
        bail!("systemctl enable --now {} failed: exit {}",
              paths::linux::SERVICE_NAME, status);
    }
    eprintln!("[sc-helper-install] systemctl enable --now OK");

    eprintln!("[sc-helper-install] done");
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn main() -> Result<()> {
    bail!("sc-helper-install: not yet implemented on this OS. Windows installer comes in phase 3.")
}

#[cfg(target_os = "linux")]
fn chown_root(path: &Path) -> Result<()> {
    // Linux: uid 0 = root, gid 0 = root (不像 mac 的 wheel).
    use nix::unistd::{chown, Gid, Uid};
    chown(path, Some(Uid::from_raw(0)), Some(Gid::from_raw(0)))
        .with_context(|| format!("chown root:root {}", path.display()))?;
    Ok(())
}

#[cfg(target_os = "linux")]
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

