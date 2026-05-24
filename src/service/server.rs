//! IPC server: accept loop on Unix socket (mac) / named pipe (win — phase 3),
//! one request per connection, dispatch into the command handler.
//!
//! Each connection lifecycle:
//!   1. Accept → spawn a task. Server keeps accepting.
//!   2. Task: read one length-prefixed frame, parse to `Request`.
//!   3. Validate `timestamp_fresh` + `verify_hmac`. Reject with `BadHmac` /
//!      `Replayed` if either fails.
//!   4. Dispatch the command. Build a `Response`.
//!   5. Serialize + write the response frame. Close.
//!
//! There's no keep-alive / connection reuse — Verge's design. Simplifies
//! state-keeping (each request stands alone) at the cost of a connect-per-call
//! which is fine for the ~1 IPC/second the GUI generates.

use crate::ipc::{
    timestamp_fresh, verify_hmac, Command, ErrorCode, Request, Response,
};
use crate::service::core::{MihomoStartConfig, SharedSupervisor};
use crate::{HELPER_BUILD_SHA, HELPER_VERSION, IPC_SECRET};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

// `read_frame`/`write_frame`/`anyhow::{Context, Result}` are only used inside
// the cfg(unix) submodule below — re-imported there rather than here so the
// non-unix build doesn't see unused-import warnings.

/// Shared "trigger to ask the accept loop to wind down". Holds an Option
/// because once fired, the sender is consumed; subsequent Shutdown IPCs
/// see None and reply ShuttingDown without re-triggering.
pub type ShutdownTrigger = Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>;

/// Build a Response for one request. Pure dispatch — no I/O, no logging.
/// I/O comes from the supervisor calls inside; logging happens at the caller.
pub async fn handle_request(
    request: Request,
    supervisor: &SharedSupervisor,
    mihomo_path_resolver: impl Fn() -> PathBuf,
    shutdown_trigger: &ShutdownTrigger,
) -> Response {
    // Replay window check first (cheaper than HMAC).
    if !timestamp_fresh(request.timestamp_nanos) {
        return Response::Error {
            code: ErrorCode::Replayed,
            message: "timestamp outside ±30s window".into(),
        };
    }
    if !verify_hmac(IPC_SECRET, &request) {
        return Response::Error {
            code: ErrorCode::BadHmac,
            message: "HMAC mismatch — helper / GUI version skew?".into(),
        };
    }

    match request.command {
        Command::Ping => Response::Pong,

        Command::GetVersion => Response::Version {
            version: HELPER_VERSION.to_string(),
            build_sha: HELPER_BUILD_SHA.to_string(),
        },

        Command::StartMihomo {
            config_dir,
            config_file,
            log_file,
        } => {
            let cfg = MihomoStartConfig {
                mihomo_path: mihomo_path_resolver(),
                config_dir: PathBuf::from(config_dir),
                config_file,
                log_file: PathBuf::from(log_file),
            };
            let mut sup = supervisor.lock().await;
            match sup
                .start(&cfg.mihomo_path, &cfg.config_dir, &cfg.config_file, &cfg.log_file)
                .await
            {
                Ok(pid) => Response::Started { pid },
                Err(e) => {
                    let already = e.to_string().contains("already running");
                    Response::Error {
                        code: if already {
                            ErrorCode::AlreadyRunning
                        } else {
                            ErrorCode::SpawnFailed
                        },
                        message: e.to_string(),
                    }
                }
            }
        }

        Command::StopMihomo => {
            let mut sup = supervisor.lock().await;
            match sup.stop().await {
                Ok(()) => Response::Stopped,
                Err(e) => Response::Error {
                    code: ErrorCode::Internal,
                    message: e.to_string(),
                },
            }
        }

        Command::SetDns { servers } => {
            #[cfg(target_os = "macos")]
            {
                match crate::service::dns::set_dns(&servers).await {
                    Ok(()) => Response::DnsSet,
                    Err(e) => Response::Error {
                        code: ErrorCode::Internal,
                        message: e.to_string(),
                    },
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                // Win uses HKCU system proxy (no DNS munging needed for the
                // 5.0.9+1 default), and TUN mode uses Wintun's own
                // dns-hijack inside mihomo. So this is intentionally a
                // no-op on Win, not an error — the GUI calls it
                // unconditionally and we want to no-op cleanly.
                let _ = servers;
                Response::DnsSet
            }
        }

        Command::ClearDns => {
            #[cfg(target_os = "macos")]
            {
                match crate::service::dns::clear_dns().await {
                    Ok(()) => Response::DnsCleared,
                    Err(e) => Response::Error {
                        code: ErrorCode::Internal,
                        message: e.to_string(),
                    },
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                Response::DnsCleared
            }
        }

        Command::Shutdown => {
            // Fire the trigger if not already fired. `take()` on the inner
            // Option means a second Shutdown IPC sees None and is a no-op
            // (we still reply ShuttingDown — idempotent from caller pov).
            let sender = shutdown_trigger.lock().await.take();
            if let Some(tx) = sender {
                let _ = tx.send(());
            }
            Response::ShuttingDown
        }
    }
}

// ---------------------------------------------------------------------------
// Unix socket server (macOS phase 0+1)
// ---------------------------------------------------------------------------

#[cfg(unix)]
pub mod unix {
    use super::*;
    use crate::ipc::{read_frame, write_frame};
    use crate::paths;
    use anyhow::{Context, Result};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use tokio::net::{UnixListener, UnixStream};
    use tokio::sync::Mutex;

    /// Resolve the socket path. Production: hard-coded /var/run path. Test:
    /// `SC_HELPER_SOCKET_OVERRIDE` env var lets the integration test pick a
    /// /tmp path so it can run as non-root without colliding with any
    /// production helper the dev may have installed.
    fn socket_path() -> String {
        std::env::var("SC_HELPER_SOCKET_OVERRIDE")
            .unwrap_or_else(|_| paths::macos::SOCKET_PATH.to_string())
    }

    /// Bind the listener, set perms, accept forever. Returns only on fatal
    /// error or on Shutdown.
    ///
    /// `shutdown_signal` is a future that resolves when the helper should
    /// exit cleanly (SIGTERM from launchd `bootout`, or IPC `Shutdown`).
    /// When it fires, the accept loop returns.
    ///
    /// The Shutdown command itself comes in via IPC, so the dispatch path
    /// needs to tell the accept loop to wind down — we do that by handing
    /// the dispatch path an `Arc<Mutex<Option<Sender<()>>>>` of a second
    /// shutdown signal. The IPC handler grabs the Option, takes the Sender,
    /// fires it; the select! below picks that up alongside the external
    /// signal.
    pub async fn serve(
        supervisor: SharedSupervisor,
        mihomo_path_resolver: impl Fn() -> PathBuf + Clone + Send + 'static,
        mut external_shutdown: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<()> {
        let socket_path = socket_path();
        let socket_path_ref: &str = socket_path.as_str();

        // Clean up a stale socket from a previous (crashed) run.
        if std::path::Path::new(socket_path_ref).exists() {
            std::fs::remove_file(socket_path_ref)
                .with_context(|| format!("removing stale socket {}", socket_path_ref))?;
        }

        let listener = UnixListener::bind(socket_path_ref)
            .with_context(|| format!("binding {}", socket_path_ref))?;

        // chmod 666 so unprivileged GUI can connect. HMAC is the auth, not
        // filesystem perms.
        std::fs::set_permissions(socket_path_ref, std::fs::Permissions::from_mode(0o666))
            .with_context(|| format!("chmod 666 {}", socket_path_ref))?;

        log::info!("sc-helper listening on {}", socket_path_ref);

        // IPC-triggered shutdown channel. Wrap the Sender in
        // Arc<Mutex<Option<...>>> so per-connection dispatch can `take()`
        // it on the first Shutdown command, and subsequent Shutdowns become
        // no-ops (single-use channel).
        let (ipc_shutdown_tx, mut ipc_shutdown_rx) =
            tokio::sync::oneshot::channel::<()>();
        let shutdown_trigger: super::ShutdownTrigger =
            Arc::new(Mutex::new(Some(ipc_shutdown_tx)));

        loop {
            tokio::select! {
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, _)) => {
                            let sup = supervisor.clone();
                            let resolver = mihomo_path_resolver.clone();
                            let trig = shutdown_trigger.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(stream, sup, resolver, trig).await {
                                    log::warn!("client connection: {e}");
                                }
                            });
                        }
                        Err(e) => {
                            log::error!("accept failed: {e}");
                            // brief backoff so we don't tight-loop if e.g.
                            // EMFILE from leaked fds.
                            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        }
                    }
                }
                _ = &mut external_shutdown => {
                    log::info!("external shutdown (SIGTERM), draining accept loop");
                    break;
                }
                _ = &mut ipc_shutdown_rx => {
                    log::info!("IPC Shutdown received, draining accept loop");
                    break;
                }
            }
        }

        // Best-effort socket cleanup so the next launch finds a clean slate.
        let _ = std::fs::remove_file(socket_path_ref);
        Ok(())
    }

    /// One connection's lifecycle: read request, dispatch, write response,
    /// close. No keep-alive.
    async fn handle_connection(
        mut stream: UnixStream,
        supervisor: SharedSupervisor,
        mihomo_path_resolver: impl Fn() -> PathBuf,
        shutdown_trigger: super::ShutdownTrigger,
    ) -> Result<()> {
        let body = read_frame(&mut stream).await?;
        let request: Request = serde_json::from_slice(&body)
            .context("parsing IPC request JSON")?;
        let response = handle_request(
            request,
            &supervisor,
            mihomo_path_resolver,
            &shutdown_trigger,
        )
        .await;
        let body = serde_json::to_vec(&response)
            .context("serializing IPC response JSON")?;
        write_frame(&mut stream, &body).await?;
        Ok(())
    }
}
