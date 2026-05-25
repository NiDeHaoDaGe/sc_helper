//! Service-side logic. Used by the `sc-helper` binary; not pulled by the
//! installer binaries (they're short-lived and have no business loading the
//! IPC server).
//!
//! Even though the helper is a long-running daemon, it has very little state:
//!
//!   * 0 or 1 `mihomo` child process. Tracked by `MihomoSupervisor`.
//!   * An IPC server accepting Unix-socket / named-pipe connections.
//!
//! That's it. No subscription handling, no YAML parsing, no rule engine —
//! all of that lives in the GUI. Keeping the helper this thin is the whole
//! point of the architecture: smaller attack surface = easier audit.

pub mod core;
// Platform-specific DNS surgery. Mac 走 networksetup (per-service DNS),
// Linux 走 resolvectl (systemd-resolved per-iface DNS). 接口一致:
//   pub async fn set_dns(servers: &[String]) -> Result<()>
//   pub async fn clear_dns() -> Result<()>
// server.rs Command::SetDns / ClearDns 调这俩.
#[cfg(target_os = "macos")]
#[path = "dns_macos.rs"]
pub mod dns;
#[cfg(target_os = "linux")]
#[path = "dns_linux.rs"]
pub mod dns;
pub mod server;
