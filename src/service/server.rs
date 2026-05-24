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
use crate::{HELPER_VERSION, IPC_SECRET};
use std::path::PathBuf;

// `read_frame`/`write_frame`/`anyhow::{Context, Result}` are only used inside
// the cfg(unix) submodule below — re-imported there rather than here so the
// non-unix build doesn't see unused-import warnings.

/// Build a Response for one request. Pure dispatch — no I/O, no logging.
/// I/O comes from the supervisor calls inside; logging happens at the caller.
pub async fn handle_request(
    request: Request,
    supervisor: &SharedSupervisor,
    mihomo_path_resolver: impl Fn() -> PathBuf,
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
            // We don't currently bake build SHA in. Phase 4 may add via build.rs.
            build_sha: String::new(),
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

        Command::SetDns { servers: _ } => {
            // Phase 1: DNS commands not yet wired (we have the +14 macOS
            // setDns code in Dart and may keep it there). Stubbed to OK so
            // GUI can call without erroring out.
            Response::DnsSet
        }

        Command::ClearDns => Response::DnsCleared,

        Command::Shutdown => Response::ShuttingDown,
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
    use tokio::net::{UnixListener, UnixStream};

    /// Bind the listener, set perms, accept forever. Returns only on fatal
    /// error.
    ///
    /// `shutdown_signal` is a future that resolves when the helper should
    /// exit cleanly (SIGTERM from launchd `bootout`, or IPC `Shutdown` from
    /// the uninstall binary). When it fires, accept loop returns.
    pub async fn serve(
        supervisor: SharedSupervisor,
        mihomo_path_resolver: impl Fn() -> PathBuf + Clone + Send + 'static,
        mut shutdown_signal: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<()> {
        let socket_path = paths::macos::SOCKET_PATH;

        // Clean up a stale socket from a previous (crashed) run.
        if std::path::Path::new(socket_path).exists() {
            std::fs::remove_file(socket_path)
                .with_context(|| format!("removing stale socket {}", socket_path))?;
        }

        let listener = UnixListener::bind(socket_path)
            .with_context(|| format!("binding {}", socket_path))?;

        // chmod 666 so unprivileged GUI can connect. HMAC is the auth, not
        // filesystem perms.
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o666))
            .with_context(|| format!("chmod 666 {}", socket_path))?;

        log::info!("sc-helper listening on {}", socket_path);

        loop {
            tokio::select! {
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, _)) => {
                            let sup = supervisor.clone();
                            let resolver = mihomo_path_resolver.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(stream, sup, resolver).await {
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
                _ = &mut shutdown_signal => {
                    log::info!("shutdown signal received, draining accept loop");
                    break;
                }
            }
        }

        // Best-effort socket cleanup so the next launch finds a clean slate.
        let _ = std::fs::remove_file(socket_path);
        Ok(())
    }

    /// One connection's lifecycle: read request, dispatch, write response,
    /// close. No keep-alive.
    async fn handle_connection(
        mut stream: UnixStream,
        supervisor: SharedSupervisor,
        mihomo_path_resolver: impl Fn() -> PathBuf,
    ) -> Result<()> {
        let body = read_frame(&mut stream).await?;
        let request: Request = serde_json::from_slice(&body)
            .context("parsing IPC request JSON")?;
        let response = handle_request(request, &supervisor, mihomo_path_resolver).await;
        let body = serde_json::to_vec(&response)
            .context("serializing IPC response JSON")?;
        write_frame(&mut stream, &body).await?;
        Ok(())
    }
}
