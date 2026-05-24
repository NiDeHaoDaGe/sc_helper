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
├── src/
│   ├── lib.rs           — IPC types, HMAC helpers (shared by all binaries)
│   ├── ipc.rs           — protocol: Request / Response, framing
│   ├── service/
│   │   ├── core.rs      — mihomo spawn / supervise / kill state machine
│   │   ├── server.rs    — IPC server loop (Unix socket / named pipe)
│   │   └── platform/
│   │       ├── macos.rs — LaunchDaemon glue, signal handling
│   │       └── windows.rs — SCM control handler, named pipe ACL
│   └── bin/
│       ├── sc_helper.rs       — the daemon binary itself
│       ├── sc_helper_install.rs   — install-time helper: write plist, register service
│       └── sc_helper_uninstall.rs — bootout + rm plist / sc delete
├── files/
│   └── com.scloud.helper.plist.tmpl  — LaunchDaemon template
└── .github/workflows/build.yml
```

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

**Phase 0** (this commit) — repo skeleton, IPC protocol spec, Mac LaunchDaemon
templates, CI scaffolding. Does **not** spawn mihomo yet, does **not** actually
listen on the socket. Just compiles + lays groundwork.

Phases tracked in the [parent project task list](../sc_win/.git/) and in
`docs/design.md`.

## License

GPL-3.0 (matching Clash Verge Rev's lineage — we lift their plist templates and
IPC framing approach).
