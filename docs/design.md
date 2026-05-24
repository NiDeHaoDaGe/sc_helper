# sc_helper design

## Decisions snapshot

| Decision | Choice | Why |
|---|---|---|
| Language | Rust | Mirrors Clash Verge upstream; cross-compiles cleanly for darwin {arm64, amd64} + win-amd64; static link drops `libc.so.6` headaches we'd hit with Go on older glibc systems |
| Scope | Full Verge parity | User's call (2026-05-24). All privileged ops route through helper, GUI is fully unprivileged |
| IPC auth | Hard-coded HMAC secret + 30s message expiry | Matches Verge; user picked over rotating-token approach. Honest "decorative security" — anyone with admin can read the binary and extract the secret; the threat model is unprivileged local code, not admin attackers |
| Platform priority | macOS first | LaunchDaemon scaffolding already in `client-macos/lib/core/mihomo_runner.dart`; smallest delta to a working MVP |
| Helper namespace | `com.scloud.helper` (brand-neutral) | Singleton on box. All branded stations (scloud / mantouyun / …) talk to the same helper |
| Socket path (mac) | `/var/run/sc-helper.sock` | `/var/run` is `root:wheel` 0755 on macOS, the socket inside gets `0666` so unprivileged GUI can connect |
| Socket path (win) | `\\.\pipe\sc-helper` | Named pipe; ACL set so `Authenticated Users` can read/write |

## IPC framing

Length-prefixed JSON, one request per connect-close (no multiplexing):

```
[4 bytes BE u32: body length] [N bytes: JSON body]
```

JSON body:

```rust
#[derive(Serialize, Deserialize)]
struct Request {
    /// monotonic nonce (unix epoch nanos) to prevent replay
    timestamp_nanos: u64,
    /// HMAC-SHA256 of (timestamp_nanos.to_be_bytes() ++ command_json), hex-encoded
    hmac: String,
    /// the actual command + payload (untagged enum)
    command: Command,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Command {
    /// Health check. No-op, returns Pong.
    Ping,
    /// Spawn mihomo with the given config dir + file. If already running, error.
    StartMihomo { config_dir: String, config_file: String, log_file: String },
    /// SIGTERM mihomo, wait up to 5s, SIGKILL if still alive.
    StopMihomo,
    /// /// Set system DNS on macOS (no-op on Win). Used by the +14 GFW-DNS workaround.
    SetDns { servers: Vec<String> },
    /// Reset system DNS back to DHCP.
    ClearDns,
    /// Helper version + build info. Used by GUI to warn "your helper is stale, please reinstall".
    GetVersion,
    /// Graceful self-exit (used by uninstaller before bootout).
    Shutdown,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Response {
    Pong,
    Started { pid: u32 },
    Stopped,
    DnsSet,
    DnsCleared,
    Version { version: String, build_sha: String },
    ShuttingDown,
    Error { code: ErrorCode, message: String },
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum ErrorCode {
    BadHmac,
    Replayed,        // timestamp > 30s old or in the future
    BadRequest,      // JSON parse failed / unknown command
    AlreadyRunning,
    NotRunning,
    SpawnFailed,
    Internal,
}
```

### HMAC computation

```
hmac = HMAC-SHA256(secret, big_endian(timestamp_nanos) || serialize_canonical(command))
```

`serialize_canonical` is `serde_json::to_vec(&command)` with `serde_json` default
ordering. Both sides use the same `serde_json` version pinned in the workspace.
**Both sides must serialize the same way** — if Dart's JSON differs from Rust's
(field order, whitespace, escaping), HMAC won't match. Mitigation: GUI builds the
JSON via a tiny canonical-JSON helper, not Dart's default `jsonEncode`.

### Secret

```rust
// src/lib.rs
pub const IPC_SECRET: &[u8] = b"sc-helper-shared-secret-please-rotate-on-major-version";
```

Rotated per major helper version. GUI ships the matching secret in its bundle.
Mismatch (e.g. helper updated but GUI didn't) returns `BadHmac` from helper; GUI
surfaces "helper version stale, reinstall required".

## Service lifecycle (mac)

1. **First install** — GUI detects `/Library/LaunchDaemons/com.scloud.helper.plist`
   missing, prompts user with `osascript do shell script "/path/to/sc_helper_install"
   with administrator privileges`.
2. `sc_helper_install` (root):
   - Copies `sc-helper` binary to `/Library/PrivilegedHelperTools/com.scloud.helper/sc-helper`
     (`0755 root:wheel`).
   - Writes `/Library/LaunchDaemons/com.scloud.helper.plist` with `RunAtLoad=true`,
     `KeepAlive=true`, `StandardOutPath`/`StandardErrorPath` to `/Library/Logs/...`.
   - `chown root:wheel` plist + `chmod 644`.
   - `launchctl bootstrap system /Library/LaunchDaemons/com.scloud.helper.plist`.
   - Exit 0.
3. Helper boots, listens on `/var/run/sc-helper.sock` (chmod 666 after bind),
   ready to take IPC.
4. **Every subsequent boot** — launchd loads at boot, helper running before GUI.
5. **Uninstall** — `sc_helper_uninstall` does `launchctl bootout` + `rm` everything.

## Service lifecycle (win — phase 3)

1. First install — GUI bundles `sc_helper_install.exe` with `requireAdministrator`
   manifest, spawns it; UAC dialog pops; user clicks Yes.
2. `sc_helper_install.exe`:
   - Copies `sc-helper.exe` to `%ProgramFiles%\sc_helper\sc-helper.exe`.
   - `sc create sc_helper binPath= "<path>" start= auto`.
   - `sc start sc_helper`.
3. Helper boots as `LocalSystem`, listens on `\\.\pipe\sc-helper`, named pipe ACL
   allows `Authenticated Users` read/write.
4. Uninstall: `sc_helper_uninstall.exe` does `sc stop` + `sc delete`.

## What does NOT live in helper

- The `mihomo` binary itself. GUI ships it inside `<App>.app/Contents/Resources/`
  / install dir, passes the path via `StartMihomo` IPC.
- Subscription handling, YAML patching, node selection, custom rules — these are
  all GUI-side concerns. Helper just gets a path to `config.yaml` and runs mihomo
  against it.
- Logs / traffic stats — mihomo's own HTTP API on `127.0.0.1:9090` serves these
  to the GUI directly. Helper doesn't proxy that.

## Failure modes worth designing for

| Scenario | Behavior |
|---|---|
| Helper not installed | GUI shows "请安装系统组件" banner, button → triggers install flow. User can dismiss → no TUN, only system proxy mode (current sidecar fallback) |
| Helper installed but socket missing | Same as not-installed; reinstall fixes |
| Helper stale (version mismatch / HMAC fail) | GUI shows "助手版本过旧" banner → reinstall offered |
| Helper crashed | `KeepAlive=true` brings it back within ~1s; GUI's next IPC retries on `ECONNREFUSED` |
| User uninstalls GUI | Helper keeps running (orphaned). Acceptable — it does nothing without IPC traffic. **TODO phase 4**: GUI uninstaller runs `sc_helper_uninstall` |
| GUI reconnect while mihomo already running | GUI calls `Ping` first; if helper reports `MihomoRunning`, GUI skips `StartMihomo` and goes straight to `MihomoController` setup |

## What we're NOT copying from Verge

- The `RunningMode { Service, Sidecar, NotRunning }` enum in the GUI. We collapse
  it to a single boolean `helper_available` — sidecar = "we run mihomo as user
  ourselves without TUN" — already handled by the existing un-elevated path.
- The C++ NSIS macros for service lifecycle. We use Inno Setup; service install
  stays in `sc_helper_install.exe`, the Inno script doesn't touch SCM.
- Verge's payload-heavy `StartClash` (clash_config object with bin_path inside).
  We pass `config_dir` + `config_file` + `log_file` — three strings — and helper
  derives `mihomo_path` from a known location next to itself, not from IPC.

## Build matrix

| Target | Triple | When |
|---|---|---|
| macOS arm64 | `aarch64-apple-darwin` | Phase 0+ |
| macOS amd64 | `x86_64-apple-darwin` | Phase 0+ |
| macOS universal | `lipo -create` | Phase 0+ (this is what GUI bundles) |
| Windows amd64-v2 | `x86_64-pc-windows-msvc` with `RUSTFLAGS=-C target-cpu=x86-64-v2` | Phase 3 |

Win7 helper: **not building one**. sc_win7 stays on per-connect elevation; the
helper requires .NET-y / modern SCM behaviors we don't want to compat-test on
Win7. Acceptable — sc_win7 is the bottom-of-barrel compat fallback; its users
don't expect Verge polish.
