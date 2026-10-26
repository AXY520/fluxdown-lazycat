//! 苹果边缘节点连通性自检：部分 A 记录（17.36.x）在国内网络路径被 RST，
//! 而 DNS 可能只返回坏 IP。启动时逐个探测，坏则从公共 DoH 找可用 IP
//! pin 进 /etc/hosts（容器内 root，ipatool 的 Go 解析器同样读到）。

use crate::{log_info, log_warn};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::time::Duration;

const PIN_MARK: &str = "# ipadown-pin";

/// 需要自检的节点（ipatool 依赖，非中国 CDN 域）
const EDGE_HOSTS: &[&str] = &["uclient-api.itunes.apple.com"];

/// 已知可用 IP 兜底候选（中国 DNS 可能只返回坏 IP，DoH 同源拿不到别的；
/// pin 前仍会探测验证，IP 失效则跳过）。发现新的可用 IP 时应更新此列表。
const KNOWN_GOOD_CANDIDATES: &[&str] = &["17.56.138.66"];

/// uclient-api 的连通性探针路径（无需认证，返回 200）
const EDGE_PROBE_PATH: &str = "/WebObjects/MZStorePlatform.woa/wa/lookup?caller=MDM&cc=cn&id=414478124&l=en&p=mdm-lockup&platform=enterprisestore&version=2";

pub async fn pin_hosts() {
    for host in EDGE_HOSTS {
        pin_host(host).await;
    }
}

/// 周期性复检：DNS 轮换、pin 的 IP 失效时自动换新或移除。由 web 启动时 spawn。
pub async fn periodic_recheck() {
    let mut ticker = tokio::time::interval(Duration::from_secs(600));
    loop {
        ticker.tick().await;
        pin_hosts().await;
    }
}

async fn pin_host(host: &str) {
    let pinned = read_hosts_pin(host);
    let system_ips: Vec<IpAddr> = (host, 443)
        .to_socket_addrs()
        .map(|addrs| addrs.map(|a| a.ip()).collect())
        .unwrap_or_default();

    // 当前 pin（或系统默认）可用 → 无事可做
    let effective = pinned.or_else(|| system_ips.first().copied());
    if let Some(ip) = effective {
        if reachable(host, &ip).await {
            return;
        }
    }

    // 候选：系统 DNS + DoH + 已知可用兜底；优先换到与当前不同的 IP
    let mut candidates = doh_lookup(host).await;
    for ip in &system_ips {
        if !candidates.contains(ip) {
            candidates.push(*ip);
        }
    }
    for raw in KNOWN_GOOD_CANDIDATES {
        if let Ok(ip) = raw.parse() {
            if !candidates.contains(&ip) {
                candidates.push(ip);
            }
        }
    }
    candidates.retain(|ip| Some(*ip) != effective);
    for ip in &candidates {
        if !reachable(host, ip).await {
            continue;
        }
        match write_hosts_pin(host, ip) {
            Ok(()) => log_info!(
                "apple-edge",
                "pinned host={host} ip={ip} (previous/default unusable)"
            ),
            Err(error) => log_warn!("apple-edge", "pin failed host={host} ip={ip} error={error}"),
        }
        return;
    }
    // 全部不可用：移除失效 pin 交还系统 DNS（无 pin 则保持现状）
    if pinned.is_some() {
        match remove_hosts_pin(host) {
            Ok(()) => log_warn!(
                "apple-edge",
                "removed stale pin host={host} (no reachable candidate, back to DNS)"
            ),
            Err(error) => {
                log_warn!(
                    "apple-edge",
                    "remove stale pin failed host={host} error={error}"
                )
            }
        }
    } else {
        log_warn!(
            "apple-edge",
            "all candidate IPs unreachable host={host} (left as-is)"
        );
    }
}

/// 对指定 IP 做 TLS 探测（resolve 固定 IP，不经过系统 DNS）
async fn reachable(host: &str, ip: &IpAddr) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .resolve(host, SocketAddr::new(*ip, 443))
        .timeout(Duration::from_secs(6))
        .build()
    else {
        return false;
    };
    let url = format!("https://{host}{EDGE_PROBE_PATH}");
    matches!(client.get(&url).send().await, Ok(resp) if resp.status().is_success())
}

/// 公共 DoH（阿里/腾讯）查 A 记录，聚合去重
async fn doh_lookup(host: &str) -> Vec<IpAddr> {
    let mut result: Vec<IpAddr> = Vec::new();
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .build()
    else {
        return result;
    };
    for base in ["https://dns.alidns.com/resolve", "https://doh.pub/resolve"] {
        let Ok(resp) = client
            .get(base)
            .query(&[("name", host), ("type", "A")])
            .send()
            .await
        else {
            continue;
        };
        let Ok(json) = resp.json::<serde_json::Value>().await else {
            continue;
        };
        if let Some(answers) = json.get("Answer").and_then(|v| v.as_array()) {
            for answer in answers {
                if let Some(ip) = answer
                    .get("data")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                {
                    if !result.contains(&ip) {
                        result.push(ip);
                    }
                }
            }
        }
        if !result.is_empty() {
            break;
        }
    }
    result
}

/// 读取当前 pin 的 IP（无则 None）
fn read_hosts_pin(host: &str) -> Option<IpAddr> {
    let content = std::fs::read_to_string("/etc/hosts").ok()?;
    content
        .lines()
        .find(|line| line.contains(host) && line.trim_end().ends_with(PIN_MARK))
        .and_then(|line| line.split_whitespace().next())
        .and_then(|ip| ip.parse().ok())
}

/// 移除 pin（交还系统 DNS）
fn remove_hosts_pin(host: &str) -> std::io::Result<()> {
    let content = std::fs::read_to_string("/etc/hosts")?;
    let kept: Vec<String> = content
        .lines()
        .filter(|line| !(line.contains(host) && line.trim_end().ends_with(PIN_MARK)))
        .map(str::to_string)
        .collect();
    std::fs::write("/etc/hosts", kept.join("\n") + "\n")
}

/// 把可用 IP 写入 /etc/hosts（容器内 root；同一 host 已有 pin 则替换）
fn write_hosts_pin(host: &str, ip: &IpAddr) -> std::io::Result<()> {
    remove_hosts_pin(host)?;
    let content = std::fs::read_to_string("/etc/hosts")?;
    let mut kept: Vec<String> = content.lines().map(str::to_string).collect();
    kept.push(format!("{ip} {host} {PIN_MARK}"));
    std::fs::write("/etc/hosts", kept.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    /// 解析出的 IP 与 DoH 结果合并去重，保证候选完整
    #[test]
    fn hosts_pin_replaces_previous_entry_for_same_host() {
        let dir = std::env::temp_dir().join("ipadown-pin-test");
        std::fs::create_dir_all(&dir).unwrap();
        let hosts_path = dir.join("hosts");
        std::fs::write(
            &hosts_path,
            "127.0.0.1 localhost\n17.36.202.106 uclient-api.itunes.apple.com # ipadown-pin\n",
        )
        .unwrap();
        let content = std::fs::read_to_string(&hosts_path).unwrap();
        const PIN_MARK: &str = "# ipadown-pin";
        let host = "uclient-api.itunes.apple.com";
        let mut kept: Vec<String> = content
            .lines()
            .filter(|line| !(line.contains(host) && line.trim_end().ends_with(PIN_MARK)))
            .map(str::to_string)
            .collect();
        kept.push(format!("17.56.138.66 {host} {PIN_MARK}"));
        let out = kept.join("\n") + "\n";
        assert_eq!(
            out,
            "127.0.0.1 localhost\n17.56.138.66 uclient-api.itunes.apple.com # ipadown-pin\n"
        );
    }
}
