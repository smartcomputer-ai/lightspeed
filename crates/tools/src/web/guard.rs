//! URL and network destination policy for web tools.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use reqwest::Url;
use tokio::net::lookup_host;

use crate::error::{ToolError, ToolResult};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WebNetworkPolicy {
    pub(crate) allow_private_networks: bool,
}

impl WebNetworkPolicy {
    pub(crate) const STRICT: Self = Self {
        allow_private_networks: false,
    };

    #[cfg(test)]
    pub(crate) const TEST_ALLOW_PRIVATE: Self = Self {
        allow_private_networks: true,
    };
}

pub(crate) async fn resolve_public_http_url(
    url: &Url,
    policy: WebNetworkPolicy,
) -> ToolResult<Vec<SocketAddr>> {
    validate_url_shape(url)?;
    validate_host_not_local_name(url)?;
    if policy.allow_private_networks {
        return Ok(Vec::new());
    }
    resolve_validated_addresses(url).await
}

fn validate_url_shape(url: &Url) -> ToolResult<()> {
    match url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(invalid_request(format!(
                "web_fetch only supports http and https URLs, got {scheme:?}"
            )));
        }
    }
    if url.host().is_none() {
        return Err(invalid_request("web_fetch URL must include a host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid_request(
            "web_fetch URLs must not contain credentials",
        ));
    }
    Ok(())
}

fn validate_host_not_local_name(url: &Url) -> ToolResult<()> {
    let Some(host) = url.host_str() else {
        return Err(invalid_request("web_fetch URL must include a host"));
    };
    let normalized = host.trim_end_matches('.').to_ascii_lowercase();
    if normalized == "localhost" || normalized.ends_with(".localhost") {
        return Err(invalid_request(
            "web_fetch rejects localhost and other non-public destinations",
        ));
    }
    Ok(())
}

async fn resolve_validated_addresses(url: &Url) -> ToolResult<Vec<SocketAddr>> {
    let Some(host) = url.host_str() else {
        return Err(invalid_request("web_fetch URL must include a host"));
    };
    let port = url.port_or_known_default().ok_or_else(|| {
        invalid_request(format!(
            "web_fetch cannot determine a port for scheme {:?}",
            url.scheme()
        ))
    })?;
    if let Ok(ip) = host.parse::<IpAddr>() {
        validate_public_ip(ip)?;
        return Ok(vec![SocketAddr::new(ip, port)]);
    }

    let addrs = lookup_host((host, port)).await.map_err(|error| {
        invalid_request(format!(
            "failed to resolve web_fetch host {host:?}: {error}"
        ))
    })?;

    let mut resolved = Vec::new();
    let mut saw_addr = false;
    for addr in addrs {
        saw_addr = true;
        validate_public_ip(addr.ip())?;
        resolved.push(addr);
    }
    if !saw_addr {
        return Err(invalid_request(format!(
            "web_fetch host {host:?} resolved to no addresses"
        )));
    }
    Ok(resolved)
}

fn validate_public_ip(ip: IpAddr) -> ToolResult<()> {
    let public = match ip {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => is_public_ipv6(ip),
    };
    if public {
        Ok(())
    } else {
        Err(invalid_request(
            "web_fetch rejects localhost and other non-public destinations",
        ))
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _d] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || a == 0
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && (18..=19).contains(&b))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113))
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    let first = segments[0];
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
        || (first == 0x2001 && segments[1] == 0x0db8)
        || ip
            .to_ipv4_mapped()
            .is_some_and(|mapped| !is_public_ipv4(mapped)))
}

fn invalid_request(message: impl Into<String>) -> ToolError {
    ToolError::InvalidRequest {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_loopback_url_before_fetch() {
        let url = Url::parse("http://127.0.0.1:8080/").expect("url");

        let error = resolve_public_http_url(&url, WebNetworkPolicy::STRICT)
            .await
            .expect_err("loopback must be rejected");

        assert!(error.to_string().contains("non-public"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_localhost_name_before_fetch() {
        let url = Url::parse("https://localhost/").expect("url");

        let error = resolve_public_http_url(&url, WebNetworkPolicy::STRICT)
            .await
            .expect_err("localhost must be rejected");

        assert!(error.to_string().contains("localhost"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_non_http_scheme() {
        let url = Url::parse("file:///etc/passwd").expect("url");

        let error = resolve_public_http_url(&url, WebNetworkPolicy::STRICT)
            .await
            .expect_err("file URL must be rejected");

        assert!(error.to_string().contains("http and https"));
    }
}
