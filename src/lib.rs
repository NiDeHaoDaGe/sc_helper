//! sc_helper shared library.
//!
//! Hosts the IPC protocol types + HMAC helpers used by all three binaries
//! (`sc-helper`, `sc-helper-install`, `sc-helper-uninstall`) and — once
//! we wire it up phase 2 — also re-implemented in Dart in `client-macos`
//! / `client` for the GUI side.
//!
//! Module layout:
//!   * `ipc` — wire format: framing, Request/Response, HMAC compute/verify
//!   * `paths` — agreed-upon filesystem paths (socket, plist, binary install dir)
//!
//! Everything else (service core, install logic) lives in the binary crates;
//! they're not reused across processes so they don't belong in `lib.rs`.

pub mod ipc;
pub mod paths;
pub mod service;

// Hard-coded HMAC secret. **Rotate on every breaking-protocol release** — when
// rotated, the GUI side ships the new secret in its bundle. Mismatch will
// surface as a `BadHmac` error and the GUI prompts the user to reinstall.
//
// This is "decorative security" per docs/design.md: it's openly committed,
// and anyone with the binary can extract it. The threat model is:
//   * Unprivileged local code (browser child process, malware running as the
//     logged-in user) trying to drive `StartMihomo` with a wrong config that
//     points to a malicious binary. The HMAC keeps **random** local code from
//     spamming the IPC socket — they'd have to read our binary first.
//   * NOT a defense against an attacker who's already root, or who already
//     has read access to our binary. Those people already win.
pub const IPC_SECRET: &[u8] =
    b"sc-helper-shared-secret-please-rotate-on-major-version-bump";

/// Crate-version string used by `GetVersion` responses + the GUI-side staleness
/// check. Picked from Cargo at compile time.
pub const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Short (12-char) git SHA of the build. Set by `build.rs`. Falls back to
/// "unknown" when built outside a git checkout (`cargo publish` tarballs,
/// downstream packagers).
pub const HELPER_BUILD_SHA: &str = env!("SC_HELPER_BUILD_SHA");
