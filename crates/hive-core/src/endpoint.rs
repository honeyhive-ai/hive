//! Where a runtime endpoint lives: on this machine or a private network, or
//! out on the internet. Hive's built-in file tools send workspace files to
//! the model's endpoint, so they are only offered without an explicit opt-in
//! when the endpoint's host is local or private.
//!
//! The endpoint is parsed with the WHATWG URL parser (the `url` crate) that
//! the HTTP client itself uses, so the host classified here is the host the
//! request goes to: `http://evil.example\@127.0.0.1/` is `evil.example`, not
//! loopback. Anything that does not parse is public.
//!
//! No DNS. Two strengths:
//!
//! - [`endpoint_is_private`] (the file-tools gate) trusts an IP address in a
//!   private range, and only the names that cannot be registered on the
//!   public internet: `localhost`, `*.localhost`, `*.local` (mDNS),
//!   `*.internal` and `*.home.arpa`. A Tailscale MagicDNS name (`*.ts.net`)
//!   is not on that list: Funnel publishes `*.ts.net` names to the internet
//!   and anyone can create one. Neither is a bare single-label name, which
//!   can be a public TLD with an address (`http://ai`).
//! - [`endpoint_is_local_server`] (whether a runtime is a keyless local
//!   server) also accepts `*.ts.net`, `*.lan` and single-label names, where
//!   a wrong guess only means a request without an API key.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Parse an endpoint the way the HTTP client will: a missing scheme means
/// `http://`, and a bare IPv6 address is bracketed. `None` when it does not
/// parse as an http(s) URL with a host.
pub fn endpoint_url(endpoint: &str) -> Option<url::Url> {
    let s = endpoint.trim();
    if s.is_empty() {
        return None;
    }
    let with_scheme = if s.contains("://") {
        s.to_string()
    } else if s.parse::<Ipv6Addr>().is_ok() {
        format!("http://[{s}]")
    } else {
        format!("http://{s}")
    };
    let url = url::Url::parse(&with_scheme).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.host()?;
    Some(url)
}

/// The host of an endpoint URL as the HTTP client will see it, lowercased,
/// without a trailing dot and (for IPv6) without brackets. `None` when the
/// endpoint does not parse or has no host.
pub fn endpoint_host(endpoint: &str) -> Option<String> {
    let url = endpoint_url(endpoint)?;
    let host = match url.host()? {
        url::Host::Domain(d) => d.trim_end_matches('.').to_ascii_lowercase(),
        url::Host::Ipv4(ip) => ip.to_string(),
        url::Host::Ipv6(ip) => ip.to_string(),
    };
    (!host.is_empty()).then_some(host)
}

/// Whether `endpoint`'s host is on this machine or a private network:
/// loopback, RFC 1918, CGNAT `100.64.0.0/10` (Tailscale), link-local, the
/// unspecified address, IPv6 unique-local `fc00::/7` (which holds Tailscale's
/// `fd7a:115c:a1e0::/48`), or one of the names in the module doc that cannot
/// be public. This is the gate for sending workspace files.
pub fn endpoint_is_private(endpoint: &str) -> bool {
    endpoint_host(endpoint).is_some_and(|h| host_is_private(&h))
}

/// [`endpoint_is_private`], or a name that is almost always a server on the
/// LAN or tailnet (`*.ts.net`, `*.lan`, a single-label name). For deciding
/// that a runtime is a keyless local server, never for sending files.
pub fn endpoint_is_local_server(endpoint: &str) -> bool {
    endpoint_host(endpoint).is_some_and(|h| host_is_private(&h) || host_is_lan_name(&h))
}

/// [`endpoint_is_private`] for a bare host (no scheme/port).
pub fn host_is_private(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return false;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return ip_is_private(ip);
    }
    if host.contains([':', '%', '/', '\\', '@']) {
        return false;
    }
    const PRIVATE_SUFFIXES: &[&str] = &[".localhost", ".local", ".internal", ".home.arpa"];
    host == "localhost" || PRIVATE_SUFFIXES.iter().any(|s| host.ends_with(s))
}

/// Names that normally resolve only on a LAN or tailnet, but could be public.
fn host_is_lan_name(host: &str) -> bool {
    if host.parse::<IpAddr>().is_ok() || host.contains([':', '%', '/', '\\', '@']) {
        return false;
    }
    host.ends_with(".ts.net")
        || host.ends_with(".lan")
        // A single-label name usually resolves through local search domains
        // or /etc/hosts — but an all-digit one is a (weird) IPv4 spelling.
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
        // WHATWG skips the extra slashes: the client connects to host "v1".
        assert_eq!(endpoint_host("http:///v1").as_deref(), Some("v1"));
        assert_eq!(endpoint_host("file:///etc/passwd"), None);
    }

    /// The host classified is the one the HTTP client connects to, for every
    /// spelling where a naive split would disagree with the URL parser.
    #[test]
    fn host_matches_the_http_client() {
        for (endpoint, client_host) in [
            ("http://evil.example\\@127.0.0.1:8080/v1/chat/completions", "evil.example"),
            ("http://api.openai.com\\@gpu-box/v1", "api.openai.com"),
            ("http://evil.example#@localhost/v1", "evil.example"),
            ("http://evil.example?@localhost/v1", "evil.example"),
            // Tabs are stripped before parsing, leaving plain userinfo.
            ("http://evil.example\t@127.0.0.1/v1", "127.0.0.1"),
            ("http://127.0.0.1%40@evil.example/v1", "evil.example"),
            ("http://localhost:8080@evil.example/v1", "evil.example"),
            ("http://%31%32%37.0.0.1:8080/v1", "127.0.0.1"),
            ("http://2130706433/v1", "127.0.0.1"),
        ] {
            let parsed = reqwest_like_host(endpoint);
            assert_eq!(parsed.as_deref(), Some(client_host), "{endpoint:?}: reference parse");
            assert_eq!(endpoint_host(endpoint).as_deref(), Some(client_host), "{endpoint:?}");
        }
    }

    /// What the HTTP client (reqwest, over the `url` crate) sees as the host.
    fn reqwest_like_host(endpoint: &str) -> Option<String> {
        let url = url::Url::parse(endpoint).ok()?;
        url.host_str().map(|h| h.trim_start_matches('[').trim_end_matches(']').to_string())
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
            "http://[::ffff:192.168.0.5]",
            "http://gpu-box.local:8080",
            "http://llm.internal",
            "http://nas.home.arpa",
            "http://2130706433",
            "http://user:pw@127.0.0.1:8080",
            "http://evil.example.com@127.0.0.1/v1",
        ] {
            assert!(endpoint_is_private(e), "{e} should be private");
            assert!(endpoint_is_local_server(e), "{e} should be a local server");
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
            "http://evil.example\\@127.0.0.1:8080/v1/chat/completions",
            "http://api.openai.com\\@gpu-box/v1",
            "http://evil.example#@localhost/v1",
            "http://evil.example?@localhost/v1",
            "http://127.0.0.1%40@evil.example/v1",
            "http://localhost:8080@evil.example/v1",
            "ftp://127.0.0.1/",
            "http://[fe80::1%25en0]:8080",
            "",
            "   ",
        ] {
            assert!(!endpoint_is_private(e), "{e:?} should be public");
            assert!(!endpoint_is_local_server(e), "{e:?} should not be a local server");
        }
    }

    /// Names that are usually on the LAN or tailnet but can be public: they
    /// do not open the file-tools gate, but still count as a keyless local
    /// server.
    #[test]
    fn lan_names_are_local_servers_but_not_private() {
        for e in [
            "http://gateway.tail1234.ts.net/v1",
            "http://gateway.tail1234.ts.net./v1",
            "https://funnel-host.tail9999.ts.net",
            "http://gpu-box:8080",
            "http://ai",
            "http://nas.lan:11434",
        ] {
            assert!(!endpoint_is_private(e), "{e} must not open the file-tools gate");
            assert!(endpoint_is_local_server(e), "{e} should still be a keyless local server");
        }
    }
}
