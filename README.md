# sc_helper

Privileged helper service for the SCloud VPN client family (sc_win / sc_mac / sc_win7).

This is a **standalone background daemon** that runs as `SYSTEM` on Windows and `root`
on macOS, exposing a local IPC endpoint to the GUI client. The GUI is unprivileged;
all root-only operations (TUN device creation, route table changes, system DNS,
`launchctl`/`sc.exe` actions, etc.) flow through this helper.

The architecture mirrors [`clash-verge-rev/clash-verge-service`](https://github.com/clash-verge-rev/clash-verge-service)
v1.1.2, with deliberate simplifications documented in [`docs/design.md`](docs/design.md).

## Why this exists

Today the SCloud Mac client `osascript`s for admin **every time** the user toggles TUN.
That's annoying and dialog-spam-trains users into typing their password without reading.
Worse, on first launch after reboot, the GUI's TUN bring-up race against `launchd`'s
own dir-creation logic has cost us two regression cycles (5.0.7+11, 5.0.7+17).

Once `sc_helper` is installed:

- The user is asked for admin **exactly once** (at install time).
- The helper auto-starts on every boot (`launchd` `KeepAlive=true` / Win SCM `Auto`).
- The GUI signs an HMAC over each IPC call and sends it over a Unix domain socket
  (mac) or named pipe (Win). The helper validates, dispatches, replies.
- TUN bring-up becomes a single IPC call, no password dialog, no UAC.

## Build

```
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin
lipo -create -output target/sc-helper-universal target/aarch64-apple-darwin/release/sc-helper target/x86_64-apple-darwin/release/sc-helper
```

CI does this automatically on macOS runner — see [`.github/workflows/build.yml`](.github/workflows/build.yml).

## Layout

```
sc_helper/
├── Cargo.toml           — workspace + dependencies
├── build.rs             — bakes git SHA into HELPER_BUILD_SHA
├── src/
│   ├── lib.rs           — IPC types, HMAC helpers (shared by all binaries)
│   ├── ipc.rs           — protocol: Request / Response, framing
│   ├── paths.rs         — socket / install / plist path constants
│   ├── service/
│   │   ├── core.rs      — mihomo spawn / supervise / kill state machine
│   │   ├── dns.rs       — networksetup wrappers (mac SetDns / ClearDns)
│   │   └── server.rs    — IPC server loop (Unix socket / named pipe phase 3)
│   └── bin/
│       ├── sc_helper.rs           — the daemon binary itself
│       ├── sc_helper_install.rs   — install-time: write plist, register service
│       ├── sc_helper_uninstall.rs — bootout + rm plist / sc delete
│       └── sc_helper_ping.rs      — debug tool: send one Ping/Version IPC
├── files/
│   └── com.scloud.helper.plist.tmpl  — LaunchDaemon template (reference)
├── tests/
│   └── integration_macos.rs  — spawns daemon, talks to it, exercises 4 commands
└── .github/workflows/build.yml
```

## Debugging an installed helper

After `sc-helper-install` puts the binaries in `/Library/PrivilegedHelperTools/com.scloud.helper/`,
the ping tool is reachable from any user account:

```
$ /Library/PrivilegedHelperTools/com.scloud.helper/sc-helper-ping
pong
$ /Library/PrivilegedHelperTools/com.scloud.helper/sc-helper-ping version
v=0.1.0 sha=982cb3a1f3d4
```

Exit code 0 = pong, 2 = helper replied with Error, 3 = unexpected response,
non-zero anywhere = connection issue (helper not running / socket missing).
Customer support tickets that say "TUN doesn't work" should start with the
output of this command.

## Brand / namespace

The helper itself is brand-neutral — same binary ships in every SCloud-family station
(`scloud`, `mantouyun`, future stations). The **bundle ID** / **service label** /
**install path** are tied to a fixed namespace (`com.scloud.helper`) regardless of
which station the GUI was branded as, because:

- Helper is a singleton on the machine. Two GUIs (e.g. `scloud` + `mantouyun` both
  installed) must talk to the **same** helper — they can't each install their own
  copy and fight over `/Library/LaunchDaemons/`.
- The IPC socket path is fixed (`/var/run/sc-helper.sock`) so any branded GUI knows
  where to connect.

If we ever need to ship a competing helper (e.g. for a forked product), bump the
namespace there. For now: one helper, one socket, all brands talk to it.

## Status

**Phase 1** (current) — Mac MVP. Daemon binds the Unix socket, dispatches all
six IPC commands. `Ping` / `GetVersion` / `StartMihomo` / `StopMihomo` /
`SetDns` (real `networksetup`) / `ClearDns` / `Shutdown` all wired. Install
+ uninstall binaries register / deregister the LaunchDaemon. Integration test
spawns the daemon, runs four commands, asserts responses. CI on macos-14
produces a universal-binary tarball + four bare binaries on every tag.

What's NOT in phase 1: sc_mac GUI doesn't talk to the helper yet (phase 2).
Windows is a stub (phase 3). No self-update / GUI-side cleanup on uninstall
(phase 4).

Phases tracked in `docs/design.md` and the parent project task list.

## License

GPL-3.0 (matching Clash Verge Rev's lineage — we lift their plist templates and
IPC framing approach).
