//! `sc-helper` — the long-running privileged daemon.
//!
//! Launched by launchd / SCM at boot. Drops into an IPC accept loop,
//! dispatches into supervisor + DNS handlers, lives forever (well,
//! until launchd sends SIGTERM or a `Shutdown` IPC comes in).
//!
//! Process model: single-threaded Tokio runtime. We have no CPU work; the
//! biggest cost is occasionally fork()-ing mihomo, which is dwarfed by
//! mihomo's own startup time.

use anyhow::Result;
use sc_helper::service::core::new_shared_supervisor;
use std::path::PathBuf;

/// Where the helper binary expects its own mihomo binary to live. Bound by
/// install layout — we DON'T let the GUI tell us "here, run /tmp/whatever as
/// root". The mihomo binary is shipped inside the helper install dir and we
/// resolve relative to the executable's own location.
///
/// On macOS the install path is
///   /Library/PrivilegedHelperTools/com.scloud.helper/sc-helper
/// so the mihomo binary would be
///   /Library/PrivilegedHelperTools/com.scloud.helper/sc-mihomo
#[cfg(target_os = "macos")]
fn resolve_mihomo_path() -> PathBuf {
    // Use the actual location of *this* exe as the anchor, so a manual move
    // (debug / reinstall to different path) still finds the sibling binary.
    let me = std::env::current_exe()
        .unwrap_or_else(|_| PathBuf::from(sc_helper::paths::macos::HELPER_BINARY));
    me.parent()
        .map(|p| p.join("sc-mihomo"))
        .unwrap_or_else(|| PathBuf::from("/Library/PrivilegedHelperTools/com.scloud.helper/sc-mihomo"))
}

#[cfg(target_os = "linux")]
fn resolve_mihomo_path() -> PathBuf {
    // 同 mac 思路: 取 *this* exe 同目录的 sc-mihomo, 不让 GUI 通过 IPC
    // 传任意 path (那等于 GUI 可以让 helper 用 root 跑任意 binary).
    // Linux install dir 是 `/usr/lib/sc-helper/`, 所以 mihomo 在
    // `/usr/lib/sc-helper/sc-mihomo`.
    let me = std::env::current_exe()
        .unwrap_or_else(|_| PathBuf::from(sc_helper::paths::linux::HELPER_BINARY));
    me.parent()
        .map(|p| p.join("sc-mihomo"))
        .unwrap_or_else(|| PathBuf::from("/usr/lib/sc-helper/sc-mihomo"))
}

#[cfg(target_os = "windows")]
#[allow(dead_code)] // Wired up in phase 3 once the named-pipe server is in.
fn resolve_mihomo_path() -> PathBuf {
    PathBuf::from(r"C:\Program Files\sc_helper\sc-mihomo.exe")
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // `RUST_LOG=info` by default — launchd captures stderr to
    // /Library/Logs/sc-helper.err.log per docs/design.md.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .init();

    log::info!("sc-helper {} starting", sc_helper::HELPER_VERSION);

    let supervisor = new_shared_supervisor();

    // Wire SIGTERM (launchd `bootout`) → shutdown_signal. The accept loop
    // watches this with `tokio::select!`.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut term = signal(SignalKind::terminate())
                .expect("registering SIGTERM handler");
            let mut int = signal(SignalKind::interrupt())
                .expect("registering SIGINT handler");
            tokio::select! {
                _ = term.recv() => log::info!("SIGTERM received"),
                _ = int.recv()  => log::info!("SIGINT received"),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            log::info!("Ctrl-C received");
        }
        let _ = shutdown_tx.send(());
    });

    #[cfg(unix)]
    {
        sc_helper::service::server::unix::serve(
            supervisor.clone(),
            resolve_mihomo_path,
            shutdown_rx,
        )
        .await?;
    }
    #[cfg(not(unix))]
    {
        // Phase 3: named pipe server. For now, exit cleanly so the build
        // produces a binary on every platform. Use references so the
        // post-loop cleanup below can still call `supervisor.lock()`.
        let _ = &supervisor;
        let _ = shutdown_rx;
        log::error!("Windows IPC server not yet implemented (phase 3)");
    }

    // Best-effort: stop any mihomo we spawned before launchd kills us.
    let mut sup = supervisor.lock().await;
    if let Err(e) = sup.stop().await {
        log::warn!("stop mihomo during shutdown: {e}");
    }

    log::info!("sc-helper exiting");
    Ok(())
}
