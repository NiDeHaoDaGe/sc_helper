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
pub mod server;
