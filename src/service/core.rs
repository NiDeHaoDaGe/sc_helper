//! Mihomo supervisor — owns the optional child process and the OS-level
//! kill/wait semantics.
//!
//! State machine:
//!
//!   * `Idle` → IPC `StartMihomo` → spawn → `Running(pid)`
//!   * `Running` → IPC `StartMihomo` → error `AlreadyRunning` (don't double-spawn)
//!   * `Running` → IPC `StopMihomo` → SIGTERM → wait 5s → SIGKILL if alive → `Idle`
//!   * `Running` → mihomo dies on its own → `Idle` (detected via wait task)
//!   * `Idle` → IPC `StopMihomo` → no-op, reply `Stopped`
//!
//! Note we deliberately do NOT auto-restart mihomo. If it dies, GUI's
//! controller ping will time out and the user sees the failure card. That's
//! more honest than silently re-spawning a process that's likely to die again
//! for the same reason (bad config / port conflict / OOM).
//!
//! The HELPER itself auto-restarts via launchd `KeepAlive=true` if it
//! crashes — that's separate from mihomo supervision.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

/// The supervisor. Wrap in `Arc<Mutex<...>>` to share across IPC handlers.
pub struct MihomoSupervisor {
    child: Option<Child>,
}

impl MihomoSupervisor {
    pub fn new() -> Self {
        Self { child: None }
    }

    /// True iff a `Child` handle is held AND the OS reports the process is
    /// still alive. We probe with `try_wait()`; if the process exited, we
    /// drop the handle and return false.
    pub async fn is_running(&mut self) -> bool {
        let Some(child) = self.child.as_mut() else {
            return false;
        };
        match child.try_wait() {
            Ok(Some(_status)) => {
                // Reaped. Drop the handle so future starts work.
                self.child = None;
                false
            }
            Ok(None) => true,
            Err(_) => {
                // try_wait errored — treat as dead, drop the handle.
                self.child = None;
                false
            }
        }
    }

    /// Start mihomo. Caller has already validated that the binary path,
    /// config dir, etc. are present on disk (we don't trust IPC strings to
    /// be paths we want to spawn).
    ///
    /// `mihomo_path` is the absolute path to the binary we'll exec. On macOS
    /// the helper resolves this relative to its own install dir (next to
    /// `/Library/PrivilegedHelperTools/com.scloud.helper/sc-helper`), so the
    /// GUI doesn't get to pick which binary runs as root — important.
    pub async fn start(
        &mut self,
        mihomo_path: &Path,
        config_dir: &Path,
        config_file: &str,
        log_file: &Path,
    ) -> Result<u32> {
        if self.is_running().await {
            return Err(anyhow!("mihomo already running"));
        }

        // Sanity-check paths before spawn — fail fast with a useful error.
        if !mihomo_path.is_file() {
            return Err(anyhow!("mihomo binary not found at {}", mihomo_path.display()));
        }
        if !config_dir.is_dir() {
            return Err(anyhow!("config_dir not a directory: {}", config_dir.display()));
        }
        let config_full = config_dir.join(config_file);
        if !config_full.is_file() {
            return Err(anyhow!("config file not found: {}", config_full.display()));
        }

        // Open the log file for append. mihomo writes both stdout + stderr
        // here.  We use std (sync) file open + into_std for Stdio.
        let log_handle_out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)
            .with_context(|| format!("opening log file {}", log_file.display()))?;
        let log_handle_err = log_handle_out.try_clone()
            .context("cloning log file handle for stderr")?;

        // -d <dir> -f <file> matches what we've always done from the GUI side.
        let child = Command::new(mihomo_path)
            .arg("-d")
            .arg(config_dir)
            .arg("-f")
            .arg(&config_full)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_handle_out))
            .stderr(Stdio::from(log_handle_err))
            .kill_on_drop(false) // we own kill explicitly
            .spawn()
            .context("spawning mihomo")?;

        let pid = child.id().ok_or_else(|| anyhow!("spawned mihomo has no pid"))?;
        self.child = Some(child);
        Ok(pid)
    }

    /// Stop mihomo gracefully. SIGTERM → wait up to 5s → SIGKILL.
    /// Idempotent: returns Ok if nothing was running.
    pub async fn stop(&mut self) -> Result<()> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };

        // Send SIGTERM (kill_on_drop=false so we have to do it manually).
        // On Unix, Tokio's `.start_kill()` sends SIGKILL — we want SIGTERM.
        // Do it via nix on macOS; on Win the equivalent is TerminateProcess.
        #[cfg(unix)]
        {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid;
            if let Some(pid) = child.id() {
                let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
            }
        }
        #[cfg(not(unix))]
        {
            // Windows: phase 3 will replace with TerminateProcess. For now,
            // fall back to start_kill so the macOS build still typechecks.
            let _ = child.start_kill();
        }

        // Wait up to 5s for the child to honor SIGTERM.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return Ok(()),
                Ok(None) => {
                    if tokio::time::Instant::now() >= deadline {
                        // Timeout — escalate to SIGKILL.
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                        return Ok(());
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// Strongly-typed config bundle passed from the IPC handler down. Lets the
/// handler validate-then-spawn in two clear steps, and the supervisor doesn't
/// need to know IPC types.
#[derive(Debug, Clone)]
pub struct MihomoStartConfig {
    pub mihomo_path: PathBuf,
    pub config_dir: PathBuf,
    pub config_file: String,
    pub log_file: PathBuf,
}

/// Wrap the supervisor in a tokio Mutex so handlers can `lock().await`.
/// Created once per process at startup.
pub type SharedSupervisor = std::sync::Arc<Mutex<MihomoSupervisor>>;

pub fn new_shared_supervisor() -> SharedSupervisor {
    std::sync::Arc::new(Mutex::new(MihomoSupervisor::new()))
}
