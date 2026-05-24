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
