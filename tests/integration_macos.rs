//! macOS-only integration test: spawn the daemon binary as a child, talk to
//! it over a private socket, expect well-formed responses.
//!
//! Skipped on Windows hosts via `#![cfg]` — the daemon's Unix-socket server
//! isn't even compiled there yet. Skipped under CI on non-macOS runners by
//! virtue of the same cfg.
//!
//! We CAN'T test the production socket path (`/var/run/sc-helper.sock`) here
//! because that requires root + collides with any helper the developer has
//! installed. Instead the test:
//!
//!   1. Builds sc-helper for the dev arch.
//!   2. Spawns it with `SC_HELPER_SOCKET_OVERRIDE=/tmp/sc-helper-test-XXX.sock`.
//!      The daemon reads this env var (added in phase 1) and binds there
//!      instead of /var/run. Falls back to /var/run when unset.
//!   3. Sends Ping → expects Pong.
//!   4. Sends GetVersion → expects matching version string.
//!   5. Sends a tampered HMAC → expects `BadHmac` error.
//!   6. Sends Shutdown → daemon exits.
//!
//! We deliberately don't test StartMihomo here because we don't have a
//! mihomo binary handy in the test fixture. Phase 2 sc_mac integration will
//! cover that end-to-end.

#![cfg(target_os = "macos")]

use sc_helper::ipc::{compute_hmac, now_nanos, read_frame, write_frame, Command, Request, Response};
use sc_helper::IPC_SECRET;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;

/// Helper: roundtrip one IPC over the given socket.
async fn send(socket: &str, command: Command) -> Response {
    let ts = now_nanos();
    let hmac = compute_hmac(IPC_SECRET, ts, &command);
    let req = Request {
        timestamp_nanos: ts,
        hmac,
        command,
    };
    let body = serde_json::to_vec(&req).unwrap();
    let mut stream = UnixStream::connect(socket).await.expect("connect");
    write_frame(&mut stream, &body).await.expect("write");
    let reply = read_frame(&mut stream).await.expect("read");
    serde_json::from_slice(&reply).expect("parse")
}

/// Same as `send` but with a deliberately wrong HMAC — used to verify the
/// daemon rejects tampered requests with `BadHmac`.
async fn send_tampered(socket: &str, command: Command) -> Response {
    let ts = now_nanos();
    let req = Request {
        timestamp_nanos: ts,
        hmac: "00".repeat(32),
        command,
    };
    let body = serde_json::to_vec(&req).unwrap();
    let mut stream = UnixStream::connect(socket).await.expect("connect");
    write_frame(&mut stream, &body).await.expect("write");
    let reply = read_frame(&mut stream).await.expect("read");
    serde_json::from_slice(&reply).expect("parse")
}

#[tokio::test(flavor = "current_thread")]
async fn daemon_round_trip() {
    // Pick a unique socket path; multiple test runs in parallel must not
    // collide. /tmp is writable in CI runners + dev machines.
    let socket = format!(
        "/tmp/sc-helper-test-{}.sock",
        std::process::id()
    );
    // Make sure we start clean.
    let _ = std::fs::remove_file(&socket);

    // Spawn the daemon binary the test harness just built.
    // `CARGO_BIN_EXE_<name>` is set by cargo for integration tests.
    let bin = env!("CARGO_BIN_EXE_sc-helper");
    let mut child = tokio::process::Command::new(bin)
        .env("SC_HELPER_SOCKET_OVERRIDE", &socket)
        .env("RUST_LOG", "info")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn daemon");

    // Drain stderr in background — keeps the daemon's pipe from filling +
    // surfaces failures if the test asserts.
    if let Some(mut stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            loop {
                match stderr.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => eprintln!("[daemon] {}", String::from_utf8_lossy(&buf[..n])),
                }
            }
        });
    }

    // Wait for the socket to appear. Daemon binds within ~50ms but we
    // give it a full second for cold start in CI.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if std::path::Path::new(&socket).exists() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("daemon socket {} did not appear within 2s", socket);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // 1. Ping → Pong
    let resp = send(&socket, Command::Ping).await;
    assert!(matches!(resp, Response::Pong), "ping: got {:?}", resp);

    // 2. GetVersion → Version with our crate version
    let resp = send(&socket, Command::GetVersion).await;
    match resp {
        Response::Version { version, build_sha } => {
            assert_eq!(version, sc_helper::HELPER_VERSION);
            // build_sha is either a real SHA or "unknown" — don't assert
            // length, just non-empty.
            assert!(!build_sha.is_empty(), "build_sha empty");
        }
        other => panic!("get_version: got {:?}", other),
    }

    // 3. Tampered HMAC → BadHmac
    let resp = send_tampered(&socket, Command::Ping).await;
    match resp {
        Response::Error { code, .. } => {
            assert_eq!(code, sc_helper::ipc::ErrorCode::BadHmac);
        }
        other => panic!("tampered ping: expected Error/BadHmac, got {:?}", other),
    }

    // 4. Shutdown — daemon replies, then exits.
    let resp = send(&socket, Command::Shutdown).await;
    assert!(matches!(resp, Response::ShuttingDown), "shutdown: got {:?}", resp);

    // Wait for the process to actually exit. 5s is plenty even on busy CI.
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    // Best-effort cleanup
    let _ = std::fs::remove_file(&socket);
}
