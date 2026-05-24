//! Bake the git SHA into the binary at compile time.
//!
//! `cargo` re-runs build.rs only when `rerun-if-changed` paths change or the
//! script itself changes. We tell it about `.git/HEAD` + `.git/refs/heads/`
//! so a commit / branch switch triggers a rebuild — but only of this script
//! and the env-var-dependent crates, not the world.
//!
//! Fallback when not inside a git checkout (e.g. `cargo publish` extracted
//! tarball, or downstream packager): emit the literal string "unknown" so
//! `GetVersion` IPC responses still parse client-side. The GUI side warns
//! on "unknown" rather than treating it as a real SHA.

use std::path::Path;
use std::process::Command;

fn main() {
    let sha = if Path::new(".git").is_dir() {
        match Command::new("git")
            .args(["rev-parse", "--short=12", "HEAD"])
            .output()
        {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            }
            _ => "unknown".to_string(),
        }
    } else {
        "unknown".to_string()
    };
    println!("cargo:rustc-env=SC_HELPER_BUILD_SHA={sha}");

    // Re-run if HEAD moves. `.git/HEAD` changes on branch switch; the file
    // under refs/heads/<branch> changes on commit. Watch both.
    println!("cargo:rerun-if-changed=.git/HEAD");
    if Path::new(".git/refs/heads").is_dir() {
        if let Ok(entries) = std::fs::read_dir(".git/refs/heads") {
            for entry in entries.flatten() {
                if let Some(p) = entry.path().to_str() {
                    println!("cargo:rerun-if-changed={p}");
                }
            }
        }
    }
}
