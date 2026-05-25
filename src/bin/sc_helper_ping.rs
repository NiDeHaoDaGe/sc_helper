//! `sc-helper-ping` — single-shot IPC client.
//!
//! Connects to the helper's socket, sends one `Ping` or `GetVersion` request,
//! prints the response. Exit code 0 on Pong, non-zero on anything else.
//!
//! Usage:
//!   sc-helper-ping              # sends Ping, prints "pong"
//!   sc-helper-ping version      # sends GetVersion, prints "v=X sha=Y"
//!
//! Shipped alongside the daemon for:
//!   * CI smoke test: after install, run this to confirm the IPC is up
//!   * Customer support: "请打开终端跑 /Library/PrivilegedHelperTools/com.scloud.helper/sc-helper-ping"
//!   * Phase 2 GUI development: known-good reference for Dart-side IPC code
//!
//! Not for production GUI use — the GUI implements its own client because
//! it needs richer error handling + async retry. This is debug-only.

use anyhow::{bail, Context, Result};
use sc_helper::ipc::{compute_hmac, now_nanos, Command, Request, Response};
#[cfg(unix)]
use sc_helper::ipc::{read_frame, write_frame};
use sc_helper::IPC_SECRET;

#[cfg(target_os = "macos")]
const SOCKET_PATH: &str = sc_helper::paths::macos::SOCKET_PATH;

#[cfg(target_os = "linux")]
const SOCKET_PATH: &str = sc_helper::paths::linux::SOCKET_PATH;

#[cfg(target_os = "windows")]
const SOCKET_PATH: &str = sc_helper::paths::windows::PIPE_NAME;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let cmd = match args.next().as_deref() {
        None | Some("ping") => Command::Ping,
        Some("version") => Command::GetVersion,
        Some(other) => bail!("unknown command {:?} (try `ping` or `version`)", other),
    };

    let response = send(cmd).await.context("sending IPC request")?;
    match response {
        Response::Pong => {
            println!("pong");
            Ok(())
        }
        Response::Version { version, build_sha } => {
            println!("v={version} sha={build_sha}");
            Ok(())
        }
        Response::Error { code, message } => {
            eprintln!("helper error: {:?} — {}", code, message);
            std::process::exit(2);
        }
        other => {
            eprintln!("unexpected response: {:?}", other);
            std::process::exit(3);
        }
    }
}

async fn send(command: Command) -> Result<Response> {
    let timestamp_nanos = now_nanos();
    let hmac = compute_hmac(IPC_SECRET, timestamp_nanos, &command);
    let request = Request {
        timestamp_nanos,
        hmac,
        command,
    };
    let body = serde_json::to_vec(&request).context("serializing request")?;

    #[cfg(unix)]
    {
        use tokio::net::UnixStream;
        let mut stream = UnixStream::connect(SOCKET_PATH)
            .await
            .with_context(|| format!("connecting to {SOCKET_PATH} (is sc-helper running?)"))?;
        write_frame(&mut stream, &body).await?;
        let reply = read_frame(&mut stream).await?;
        let response: Response =
            serde_json::from_slice(&reply).context("deserializing response")?;
        Ok(response)
    }

    #[cfg(not(unix))]
    {
        let _ = (body, SOCKET_PATH);
        anyhow::bail!("named-pipe ping client not implemented yet (phase 3)")
    }
}
