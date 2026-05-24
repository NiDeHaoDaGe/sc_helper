//! IPC wire format.
//!
//! See [`docs/design.md`] for the rationale. Brief recap:
//!
//!   * Single request → single response per socket connect. No multiplexing.
//!   * Both directions are length-prefixed JSON:
//!         [4 bytes BE u32: body length] [N bytes: JSON body]
//!   * Request carries `timestamp_nanos` (replay prevention) + `hmac` (auth)
//!     + `command` (the actual payload).
//!   * HMAC is computed over `(timestamp_nanos as big-endian u64) || canonical_json(command)`.
//!     Both sides must compute `canonical_json` identically. We use
//!     `serde_json::to_vec(&command)` with serde's default field ordering;
//!     the Dart GUI side replicates this by emitting fields in declaration
//!     order, no whitespace. If we ever rename fields we'd break wire compat —
//!     bump `IPC_SECRET` (which signals "your GUI is stale, reinstall").
//!
//! Frame size cap: 1 MiB. Anything over that is a sign of a confused client
//! (or a malicious one trying to OOM us). Helper closes the connection.

use serde::{Deserialize, Serialize};

/// Hard cap on JSON body length. Real requests are well under 4 KiB.
/// 1 MiB lets us be lenient with debug payloads while still rejecting garbage.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Replay-prevention window. Requests with `timestamp_nanos` outside
/// `[now - 30s, now + 30s]` are rejected.
pub const REPLAY_WINDOW_NANOS: u64 = 30 * 1_000_000_000;

#[derive(Serialize, Deserialize, Debug)]
pub struct Request {
    pub timestamp_nanos: u64,
    /// Hex-encoded HMAC-SHA256, lowercase. 64 chars.
    pub hmac: String,
    pub command: Command,
}

/// Each variant is one operation the GUI can ask the helper to do.
///
/// `serde(tag = "kind")` means JSON looks like
///   `{"kind": "start_mihomo", "config_dir": "...", "config_file": "...", "log_file": "..."}`
/// rather than the noisier `{"StartMihomo": {...}}` you'd get without it.
/// This is also what Verge does, so Dart-side serialization stays simple.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Command {
    /// Health check. Helper replies [`Response::Pong`].
    Ping,

    /// Start mihomo with the given config. If already running, returns
    /// [`ErrorCode::AlreadyRunning`].
    StartMihomo {
        /// Absolute path to the directory containing config.yaml + geodata.
        /// Passed to mihomo as `-d <config_dir>`.
        config_dir: String,
        /// Filename inside `config_dir`. Passed as `-f <config_dir>/<config_file>`.
        config_file: String,
        /// stdout/stderr destination for mihomo's logs. Helper redirects both.
        log_file: String,
    },

    /// SIGTERM mihomo, wait up to 5s, SIGKILL if still alive. Idempotent: if
    /// mihomo isn't running, replies [`Response::Stopped`] anyway.
    StopMihomo,

    /// Set the active network service's DNS to the given list (macOS only).
    /// On Windows this is a no-op (returns [`Response::DnsSet`]) because the
    /// 5.0.9+1 default already uses HKCU system proxy, no DNS override needed.
    SetDns { servers: Vec<String> },

    /// Reset DNS back to DHCP for all network services.
    ClearDns,

    /// Helper version + git SHA. GUI uses this to detect "your helper is stale".
    GetVersion,

    /// Graceful self-exit. Used by `sc-helper-uninstall` immediately before
    /// `launchctl bootout` to avoid SIGKILL race.
    Shutdown,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Pong,
    Started { pid: u32 },
    Stopped,
    DnsSet,
    DnsCleared,
    Version { version: String, build_sha: String },
    ShuttingDown,
    Error { code: ErrorCode, message: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// HMAC didn't match. GUI is stale or secret mismatch.
    BadHmac,
    /// Timestamp outside replay window.
    Replayed,
    /// JSON parse failed or unknown command.
    BadRequest,
    /// `StartMihomo` while mihomo is already running.
    AlreadyRunning,
    /// `StopMihomo` while no mihomo is running. (Note: we still return
    /// `Stopped`, not this error — kept for completeness in case future
    /// commands need it.)
    NotRunning,
    /// `Command::Process::spawn` failed; message has the OS error.
    SpawnFailed,
    /// Anything else; message has the detail.
    Internal,
}

// ---------------------------------------------------------------------------
// HMAC compute / verify
// ---------------------------------------------------------------------------

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Serialize `command` to canonical JSON bytes — same form both sides must
/// agree on. serde_json's default output (no whitespace, fields in struct
/// declaration order) is the canonical form.
pub fn canonical_command_bytes(command: &Command) -> Vec<u8> {
    serde_json::to_vec(command).expect("Command serialization is infallible")
}

/// Compute the HMAC for a (timestamp_nanos, command) pair. Returns lowercase
/// hex-encoded SHA256 digest (64 chars).
pub fn compute_hmac(secret: &[u8], timestamp_nanos: u64, command: &Command) -> String {
    let mut mac = HmacSha256::new_from_slice(secret)
        .expect("HMAC accepts arbitrary key length");
    mac.update(&timestamp_nanos.to_be_bytes());
    mac.update(&canonical_command_bytes(command));
    let digest = mac.finalize().into_bytes();
    hex::encode(digest)
}

/// Constant-time compare a request's claimed HMAC against the freshly
/// computed one. Returns true iff they match.
pub fn verify_hmac(secret: &[u8], request: &Request) -> bool {
    let expected = compute_hmac(secret, request.timestamp_nanos, &request.command);
    // hex::decode for ct compare; falling back to byte compare if either side
    // is malformed. `subtle` crate would be tidier but pulling it in for one
    // 32-byte compare is overkill.
    let claimed = match hex::decode(&request.hmac) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let expected_bytes = match hex::decode(&expected) {
        Ok(b) => b,
        Err(_) => return false,
    };
    if claimed.len() != expected_bytes.len() {
        return false;
    }
    // Manual constant-time compare.
    let mut diff: u8 = 0;
    for i in 0..claimed.len() {
        diff |= claimed[i] ^ expected_bytes[i];
    }
    diff == 0
}

/// Current wall-clock in unix-epoch nanoseconds. Used by both sides as the
/// `timestamp_nanos` field. Caller is responsible for ensuring system clock
/// hygiene — we don't try to sync our own.
pub fn now_nanos() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock predates 1970 — refusing to continue")
        .as_nanos() as u64
}

/// Decide if a request's `timestamp_nanos` is within the replay window
/// relative to the current clock. Allows symmetric drift (request could be
/// from a slightly-ahead client clock).
pub fn timestamp_fresh(timestamp_nanos: u64) -> bool {
    let now = now_nanos();
    let diff = if now > timestamp_nanos {
        now - timestamp_nanos
    } else {
        timestamp_nanos - now
    };
    diff <= REPLAY_WINDOW_NANOS
}

// ---------------------------------------------------------------------------
// Framing helpers — async read/write of length-prefixed JSON
// ---------------------------------------------------------------------------

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Read one length-prefixed JSON frame from a stream. Returns the raw bytes
/// (caller deserializes — different binaries care about different types).
pub async fn read_frame<R: AsyncReadExt + Unpin>(reader: &mut R) -> anyhow::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        anyhow::bail!("frame too large: {} bytes (cap {})", len, MAX_FRAME_BYTES);
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await?;
    Ok(body)
}

/// Write a length-prefixed JSON frame.
pub async fn write_frame<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    body: &[u8],
) -> anyhow::Result<()> {
    if body.len() > MAX_FRAME_BYTES {
        anyhow::bail!("frame too large: {} bytes (cap {})", body.len(), MAX_FRAME_BYTES);
    }
    let len = (body.len() as u32).to_be_bytes();
    writer.write_all(&len).await?;
    writer.write_all(body).await?;
    writer.flush().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_round_trip() {
        let secret = b"test-secret";
        let cmd = Command::Ping;
        let ts = 1234567890_u64;
        let mac = compute_hmac(secret, ts, &cmd);
        let req = Request {
            timestamp_nanos: ts,
            hmac: mac,
            command: cmd,
        };
        assert!(verify_hmac(secret, &req));
    }

    #[test]
    fn hmac_reject_tampered_command() {
        let secret = b"test-secret";
        let cmd_a = Command::Ping;
        let cmd_b = Command::StopMihomo;
        let ts = 1_u64;
        let mac = compute_hmac(secret, ts, &cmd_a);
        let req = Request {
            timestamp_nanos: ts,
            hmac: mac,
            command: cmd_b, // <-- mismatch
        };
        assert!(!verify_hmac(secret, &req));
    }

    #[test]
    fn hmac_reject_wrong_secret() {
        let cmd = Command::Ping;
        let ts = 1_u64;
        let mac = compute_hmac(b"secret-a", ts, &cmd);
        let req = Request {
            timestamp_nanos: ts,
            hmac: mac,
            command: cmd,
        };
        assert!(!verify_hmac(b"secret-b", &req));
    }

    #[test]
    fn timestamp_fresh_recent_ok() {
        assert!(timestamp_fresh(now_nanos()));
    }

    #[test]
    fn timestamp_fresh_far_past_rejected() {
        assert!(!timestamp_fresh(now_nanos().saturating_sub(60_000_000_000)));
    }

    #[test]
    fn canonical_command_stable() {
        // Two serializations of the same command must produce identical bytes —
        // the GUI side is going to replicate this in Dart.
        let cmd = Command::StartMihomo {
            config_dir: "/tmp/x".into(),
            config_file: "config.yaml".into(),
            log_file: "/tmp/y.log".into(),
        };
        let a = canonical_command_bytes(&cmd);
        let b = canonical_command_bytes(&cmd);
        assert_eq!(a, b);
        // Spot-check the byte order so a future serde upgrade doesn't silently
        // change the field order.
        let s = String::from_utf8(a).unwrap();
        assert!(s.starts_with(r#"{"kind":"start_mihomo","config_dir":"/tmp/x""#));
    }
}
