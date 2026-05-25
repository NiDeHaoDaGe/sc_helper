//! Filesystem paths that are agreed-upon between helper, install binary,
//! uninstall binary, and the GUI client. Centralised here so a typo can only
//! happen in one place.
//!
//! These paths are deliberately **brand-neutral** — the helper is a singleton
//! on the box and serves every SCloud-family GUI, so we use `sc-helper` /
//! `com.scloud.helper` regardless of which station the GUI was branded as.

#[cfg(target_os = "macos")]
pub mod macos {
    /// LaunchDaemon label. Must match the `Label` key in the plist.
    pub const LAUNCHD_LABEL: &str = "com.scloud.helper";

    /// LaunchDaemon plist path. `launchctl bootstrap system <this>` registers,
    /// `launchctl bootout system/<label>` unregisters.
    pub const LAUNCHD_PLIST: &str =
        "/Library/LaunchDaemons/com.scloud.helper.plist";

    /// Where the install binary copies `sc-helper` to. Apple convention says
    /// privileged helpers live under `/Library/PrivilegedHelperTools/<bundle_id>/`.
    pub const HELPER_INSTALL_DIR: &str =
        "/Library/PrivilegedHelperTools/com.scloud.helper";

    /// The helper binary itself (under [`HELPER_INSTALL_DIR`]).
    pub const HELPER_BINARY: &str =
        "/Library/PrivilegedHelperTools/com.scloud.helper/sc-helper";

    /// stdout / stderr captured by launchd. `/Library/Logs` is world-readable
    /// (helpful for debugging) but only `root:wheel` can write — exactly what
    /// we want.
    pub const STDOUT_LOG: &str = "/Library/Logs/sc-helper.log";
    pub const STDERR_LOG: &str = "/Library/Logs/sc-helper.err.log";

    /// IPC socket path. `/var/run` is a symlink to `/private/var/run` and is
    /// `root:wheel 0755` by default. We bind the socket inside as `root` then
    /// `chmod 0666` so unprivileged GUIs can connect. Read-only-to-others is
    /// not a useful security boundary here because the HMAC is doing that job.
    pub const SOCKET_PATH: &str = "/var/run/sc-helper.sock";
}

#[cfg(target_os = "linux")]
pub mod linux {
    /// systemd unit name. `systemctl <action> sc-helper` 操作 service.
    pub const SERVICE_NAME: &str = "sc-helper";

    /// systemd unit file path. `systemctl daemon-reload` 之后 enable.
    pub const SYSTEMD_UNIT_PATH: &str = "/etc/systemd/system/sc-helper.service";

    /// Linux 上 privileged helper 通常放 `/usr/lib/<service>/` (跟
    /// NetworkManager, systemd 自家 plugin 一致). `/Library/PrivilegedHelperTools/`
    /// 是 macOS specific, Linux 不用. `/opt/` 是 third-party 偏好 — 我们
    /// 走 distro-friendly 的 `/usr/lib/`.
    pub const HELPER_INSTALL_DIR: &str = "/usr/lib/sc-helper";

    /// The helper binary itself.
    pub const HELPER_BINARY: &str = "/usr/lib/sc-helper/sc-helper";

    /// stdout / stderr — systemd 默认把 service 的 stdout/stderr 捕获到
    /// journald. 我们仍然 declare 文件 path 是给 RUST_LOG 想 redirect
    /// 到独立 file 时备用 (不是默认行为). 默认走 journald, `journalctl
    /// -u sc-helper -f` 看 log.
    pub const STDOUT_LOG: &str = "/var/log/sc-helper.log";
    pub const STDERR_LOG: &str = "/var/log/sc-helper.err.log";

    /// IPC socket path. `/run` 是现代 Linux 标准 (相比老 `/var/run`,
    /// `/var/run` 现在 typically 是个 symlink to `/run`). 默认 `root:root
    /// 0755` 目录, helper bind 后 `chmod 0666` socket 让 unprivileged
    /// GUI 能连. HMAC 是 auth, 不靠文件权限.
    pub const SOCKET_PATH: &str = "/run/sc-helper.sock";
}

#[cfg(target_os = "windows")]
pub mod windows {
    /// SCM service name. `sc create <this>` / `sc delete <this>`.
    pub const SERVICE_NAME: &str = "sc_helper";

    /// SCM display name shown in `services.msc`.
    pub const SERVICE_DISPLAY: &str = "SCloud VPN Helper";

    /// Where the install binary copies `sc-helper.exe` to.
    pub const HELPER_INSTALL_DIR: &str = r"C:\Program Files\sc_helper";

    pub const HELPER_BINARY: &str = r"C:\Program Files\sc_helper\sc-helper.exe";

    /// Named pipe path. Phase 3.
    pub const PIPE_NAME: &str = r"\\.\pipe\sc-helper";
}
