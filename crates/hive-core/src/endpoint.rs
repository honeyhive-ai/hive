//! Where a runtime endpoint lives: on this machine or a private network, or
//! out on the internet. Hive's built-in file tools send workspace files to
//! the model's endpoint, so they are only offered without an explicit opt-in
//! when the endpoint's host is local or private.
//!
//! Pure string/IP classification — no DNS. A hostname is private only by
//! name (`localhost`, `*.local`, `*.ts.net`, a bare single-label name); any
//! other name counts as public even if it happens to resolve to a LAN
//! address, which errs on the side of asking.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The host part of an endpoint URL (`http://host:port/path`, with or without
/// a scheme, IPv6 in brackets, userinfo ignored), lowercased, without a
/// trailing dot. `None` when there is no host.
pub fn endpoint_host(endpoint: &str) -> Option<String> {
    let s = endpoint.trim();
    let rest = match s.find("://") {
        Some(i) => &s[i + 3..],
        None => s,
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = authority.rsplit('@').next().unwrap_or("");
    let host = if let Some(stripped) = hostport.strip_prefix('[') {
        stripped.split(']').next().unwrap_or("")
    } else if hostport.matches(':').count() > 1 {
        // A bare IPv6 address without brackets (no port possible).
        hostport
    } else {
        hostport.split(':').next().unwrap_or("")
    };
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// Whether `endpoint`'s host is on this machine or a private network:
/// loopback, RFC 1918, CGNAT `100.64.0.0/10` (Tailscale), link-local, the
/// unspecified address, IPv6 unique-local `fc00::/7` (which holds Tailscale's
/// `fd7a:115c:a1e0::/48`), `localhost`, `*.localhost`, `*.local` (mDNS),
/// `*.ts.net` (Tailscale MagicDNS), `*.internal`, `*.lan`, `*.home.arpa`, and
/// bare single-label names (`gpu-box`), which only resolve on a LAN.
pub fn endpoint_is_private(endpoint: &str) -> bool {
    endpoint_host(endpoint).is_some_and(|h| host_is_private(&h))
}

/// [`endpoint_is_private`] for a bare host (no scheme/port).
pub fn host_is_private(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return false;
    }
    // Zone-scoped IPv6 literals (`fe80::1%en0`) are link-local by definition.
    let ip_part = host.split('%').next().unwrap_or(host);
    if let Ok(ip) = ip_part.parse::<IpAddr>() {
        return ip_is_private(ip);
    }
    if host.contains(':') {
        return false;
    }
    const PRIVATE_SUFFIXES: &[&str] = &[".localhost", ".local", ".ts.net", ".internal", ".lan", ".home.arpa"];
    host == "localhost"
        || PRIVATE_SUFFIXES.iter().any(|s| host.ends_with(s))
        // A single-label name only resolves through local search domains or
        // /etc/hosts — but an all-digit one is a (weird) IPv4 spelling.
        || (!host.contains('.') && !host.chars().all(|c| c.is_ascii_digit()))
}

fn ip_is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => ipv4_is_private(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => ipv4_is_private(v4),
            None => ipv6_is_private(v6),
        },
    }
}

fn ipv4_is_private(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        // CGNAT 100.64.0.0/10 — Tailscale's address space.
        || (a == 100 && (64..=127).contains(&b))
}

fn ipv6_is_private(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    ip.is_loopback()
        || ip.is_unspecified()
        // Unique local fc00::/7 (Tailscale's fd7a:115c:a1e0::/48 is in it).
        || (first & 0xfe00) == 0xfc00
        // Link-local fe80::/10.
        || (first & 0xffc0) == 0xfe80
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_the_host() {
        assert_eq!(endpoint_host("http://100.100.10.20:8080/v1/chat/completions").as_deref(), Some("100.100.10.20"));
        assert_eq!(endpoint_host("https://API.OpenAI.com/v1").as_deref(), Some("api.openai.com"));
        assert_eq!(endpoint_host("localhost:11434").as_deref(), Some("localhost"));
        assert_eq!(endpoint_host("http://[::1]:8080/v1").as_deref(), Some("::1"));
        assert_eq!(endpoint_host("http://user:pw@gpu.local:8080").as_deref(), Some("gpu.local"));
        assert_eq!(endpoint_host("http://box.tail1234.ts.net./v1").as_deref(), Some("box.tail1234.ts.net"));
        assert_eq!(endpoint_host("fd7a:115c:a1e0::1").as_deref(), Some("fd7a:115c:a1e0::1"));
        assert_eq!(endpoint_host(""), None);
        assert_eq!(endpoint_host("http:///v1"), None);
    }

    #[test]
    fn local_and_private_hosts() {
        for e in [
            "http://localhost:11434",
            "http://LOCALHOST:8080/v1/chat/completions",
            "http://ollama.localhost/v1",
            "http://127.0.0.1:8080",
            "http://127.8.9.10",
            "http://0.0.0.0:8080",
            "http://10.0.0.2:8080/v1/chat/completions",
            "http://172.16.0.1",
            "http://172.31.255.255",
            "http://192.168.1.20:11434",
            "http://169.254.10.1",
            "http://100.64.0.1",
            "http://100.100.10.20:8080/v1/chat/completions",
            "http://100.127.255.254",
            "http://[::1]:8080",
            "http://[fd7a:115c:a1e0::1234]:8080",
            "http://[fd12:3456::1]",
            "http://[fe80::1%25en0]:8080",
            "http://[::ffff:192.168.0.5]",
            "http://gpu-box.local:8080",
            "http://gateway.tail1234.ts.net/v1",
            "http://gateway.tail1234.ts.net./v1",
            "http://gpu-box:8080",
            "http://llm.internal",
            "http://nas.home.arpa",
        ] {
            assert!(endpoint_is_private(e), "{e} should be private");
        }
    }

    #[test]
    fn public_hosts() {
        for e in [
            "https://api.openai.com/v1/chat/completions",
            "https://openrouter.ai/api/v1",
            "https://my-resource.openai.azure.com",
            "http://8.8.8.8",
            "http://100.63.255.255",
            "http://100.128.0.1",
            "http://172.15.0.1",
            "http://172.32.0.1",
            "http://192.169.0.1",
            "http://[2001:4860:4860::8888]",
            "http://[::ffff:8.8.8.8]",
            "http://local.example.com",
            "http://example.local.evil.com",
            "http://ts.net.evil.com",
            "http://localhost.evil.com",
            "http://2130706433",
            "",
            "   ",
        ] {
            assert!(!endpoint_is_private(e), "{e:?} should be public");
        }
    }
}
