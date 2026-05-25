//! Linux DNS surgery via `resolvectl` (systemd-resolved).
//!
//! 跟 mac dns_macos.rs 同款语义, 实现不同:
//!   * mac: `networksetup -setdnsservers <service> <ip ...>` per network
//!     service (Wi-Fi / Ethernet / 等), 然后 `dscacheutil -flushcache`.
//!   * Linux: `resolvectl dns <iface> <ip ...>` per network interface
//!     (默认路由 iface), 然后 `resolvectl flush-caches`.
//!
//! 为什么走 resolvectl 而不是 写 `/etc/resolv.conf`:
//!   * 现代 Linux (Debian 12 / Ubuntu 18.04+ / Fedora / Arch + NetworkManager)
//!     `/etc/resolv.conf` 是 symlink 到 `/run/systemd/resolve/stub-resolv.conf`,
//!     直接写文件被 systemd-resolved 覆盖.
//!   * resolvectl 是 systemd-resolved 官方 API, 设置 per-iface DNS + 自动
//!     widely cached + GFW 环境 split-DNS 用 domain 修饰可以控.
//!   * 也跟 GUI 端 sc_linux/lib/core/system_proxy.dart::setDns 同款机制
//!     (那边走 user-mode resolvectl 但 best-effort, helper 这边 root 跑
//!     保证生效).
//!
//! GFW 环境用法 (跟 mac +14 同款): TUN 模式时 GUI 调
//!   SetDns(["198.18.0.1"])
//! 强制 system DNS 走 utun gateway, 让 mihomo dns-hijack 截获. 不走 utun
//! 的 LAN DNS 会被 GFW 上游污染, 域名解析坏.
//!
//! systemd-resolved 不存在的 fallback (老发行版 / Devuan / Alpine):
//! resolvectl exit 127 "command not found", 我们 anyhow error 上去, GUI
//! 显示 "Linux DNS 设置失败" — 不致命, 用户可以手动跑或者忽略 (TUN 模式
//! 没 DNS hijack 也能用, 只是部分 GFW 污染域名解析失败).

#![cfg(target_os = "linux")]

use anyhow::{Context, Result};
use tokio::process::Command;

/// 解析 `ip route get 1.1.1.1` 拿到默认路由出口 interface name.
///
/// 输出 like: `1.1.1.1 via 192.168.1.1 dev wlp3s0 src 192.168.1.42 uid 0`
/// 抽 `dev <name>` 那一段拿 iface (eth0 / enp3s0 / wlp2s0 / wlan0).
/// 1.1.1.1 是 dummy 公网目的地 — 触发 OS 路由表查询, 不真发包.
async fn default_iface() -> Result<String> {
    let out = Command::new("ip")
        .args(["route", "get", "1.1.1.1"])
        .output()
        .await
        .context("spawning `ip route get 1.1.1.1`")?;
    if !out.status.success() {
        anyhow::bail!(
            "`ip route get 1.1.1.1` exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim(),
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    // 找 " dev <name>" — 简单 split / windows 都行. 直接 token scan:
    let mut tokens = stdout.split_whitespace();
    while let Some(tok) = tokens.next() {
        if tok == "dev" {
            if let Some(iface) = tokens.next() {
                return Ok(iface.to_string());
            }
        }
    }
    anyhow::bail!("no `dev <iface>` in `ip route get 1.1.1.1` output: {}", stdout.trim());
}

/// 设 system DNS = `servers` for default-route iface. Empty 列表拒掉
/// (调用方应该用 clear_dns 而不是 empty set).
pub async fn set_dns(servers: &[String]) -> Result<()> {
    if servers.is_empty() {
        anyhow::bail!("set_dns called with empty servers; use clear_dns() to revert to DHCP");
    }
    let iface = default_iface().await?;
    let mut cmd = Command::new("resolvectl");
    cmd.arg("dns").arg(&iface);
    for s in servers {
        cmd.arg(s);
    }
    let out = cmd.output().await.context("spawning resolvectl dns")?;
    if !out.status.success() {
        anyhow::bail!(
            "resolvectl dns failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    log::info!("set DNS on iface {:?} to {:?}", iface, servers);

    // `domain ~.` 让 systemd-resolved 把所有域名优先查这个 iface 的 DNS
    // (默认 split-DNS 行为可能只查局部域名). TUN 模式我们要全局接管,
    // 必须 ~. 修饰.
    let out = Command::new("resolvectl")
        .args(["domain", &iface, "~."])
        .output()
        .await
        .context("spawning resolvectl domain")?;
    if !out.status.success() {
        // 不致命 — DNS set 成功, domain 修饰失败只是 split-DNS 还在.
        log::warn!(
            "resolvectl domain ~. failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    flush_cache().await?;
    Ok(())
}

/// 还原 DNS 给 DHCP 提供的值. `resolvectl revert <iface>` 清除我们 set
/// 过的 DNS + domain, NetworkManager / systemd-networkd 重新 DHCP.
pub async fn clear_dns() -> Result<()> {
    let iface = default_iface().await?;
    let out = Command::new("resolvectl")
        .args(["revert", &iface])
        .output()
        .await
        .context("spawning resolvectl revert")?;
    if !out.status.success() {
        anyhow::bail!(
            "resolvectl revert failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    log::info!("cleared DNS on iface {:?} (reverted to DHCP)", iface);
    flush_cache().await?;
    Ok(())
}

/// 清 systemd-resolved 的 DNS 缓存. 跟 mac `dscacheutil -flushcache`
/// 同款目的 — 切了 DNS server 之后, OS resolver 还可能 cached 5-30 分钟
/// 旧记录, 用户感觉 "DNS 改了但 GFW 还是污染". 强制 flush.
async fn flush_cache() -> Result<()> {
    let out = Command::new("resolvectl")
        .arg("flush-caches")
        .output()
        .await
        .context("spawning resolvectl flush-caches")?;
    if !out.status.success() {
        // 不致命 — DNS set 成功了, cache flush 失败用户 retry 几次会过.
        log::warn!(
            "resolvectl flush-caches failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}
