//! Bounded, DNS-pinned outbound HTTP for MCP and MCP OAuth.
//!
//! The caller supplies the product's private-network policy. Every request and
//! redirect hop is resolved independently, validated, and pinned into a
//! redirect-disabled `reqwest` client before any bytes leave the process.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use ipnet::IpNet;
use reqwest::{Method, StatusCode, Url};
use rmcp::transport::auth::{
    OAuthHttpClient, OAuthHttpClientError, OAuthHttpClientFuture, OAuthHttpRedirectPolicy,
    OAuthHttpRequest,
};

pub const DEFAULT_OAUTH_HTTP_MAX_BODY_BYTES: usize = 1024 * 1024;
pub const DEFAULT_OAUTH_HTTP_MAX_HEADER_BYTES: usize = 64 * 1024;
pub const DEFAULT_OAUTH_HTTP_MAX_REDIRECTS: usize = 5;

/// Which non-public destinations a pinned client may reach.
///
/// The decision is made per resolved address, after DNS, so a hostname is
/// judged by where it actually points and the connection is pinned to the
/// addresses that passed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PrivateNetworkAccess {
    /// Public addresses only, over HTTPS.
    #[default]
    None,
    /// Public addresses over HTTPS, plus any address inside these networks
    /// over HTTP or HTTPS.
    Networks(Arc<[IpNet]>),
    /// Any address the process can reach, over HTTP or HTTPS.
    Any,
}

impl PrivateNetworkAccess {
    fn admits(&self, ip: IpAddr) -> bool {
        match self {
            Self::None => false,
            Self::Networks(networks) => {
                // An IPv4-mapped IPv6 address is the IPv4 destination it names.
                let ip = ip.to_canonical();
                networks.iter().any(|network| network.contains(&ip))
            }
            Self::Any => true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PinnedHttpPolicy {
    private_access: PrivateNetworkAccess,
    timeout: Duration,
    max_body_bytes: usize,
    max_header_bytes: usize,
    max_redirects: usize,
}

impl PinnedHttpPolicy {
    pub fn public_only() -> Self {
        Self {
            private_access: PrivateNetworkAccess::None,
            timeout: Duration::from_secs(30),
            max_body_bytes: DEFAULT_OAUTH_HTTP_MAX_BODY_BYTES,
            max_header_bytes: DEFAULT_OAUTH_HTTP_MAX_HEADER_BYTES,
            max_redirects: DEFAULT_OAUTH_HTTP_MAX_REDIRECTS,
        }
    }

    pub fn allowing_private_networks() -> Self {
        Self::public_only().with_private_access(PrivateNetworkAccess::Any)
    }

    pub fn with_private_access(mut self, private_access: PrivateNetworkAccess) -> Self {
        self.private_access = private_access;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_max_body_bytes(mut self, max_body_bytes: usize) -> Self {
        self.max_body_bytes = max_body_bytes.max(1);
        self
    }

    pub fn with_max_header_bytes(mut self, max_header_bytes: usize) -> Self {
        self.max_header_bytes = max_header_bytes.max(1);
        self
    }

    pub fn with_max_redirects(mut self, max_redirects: usize) -> Self {
        self.max_redirects = max_redirects;
        self
    }

    pub async fn client_for_url(&self, url: &Url) -> Result<reqwest::Client, PinnedHttpError> {
        self.validate_scheme(url)?;
        if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
            return Err(PinnedHttpError::InvalidUrl);
        }
        let host = url.host_str().ok_or(PinnedHttpError::InvalidUrl)?;
        let port = url
            .port_or_known_default()
            .ok_or(PinnedHttpError::InvalidUrl)?;
        // An IP literal is its own resolution; the URL writes IPv6 in brackets,
        // which a resolver would treat as a name.
        let literal = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>();
        let resolved: Vec<SocketAddr> = match literal {
            Ok(ip) => vec![SocketAddr::new(ip, port)],
            Err(_) => tokio::time::timeout(self.timeout, tokio::net::lookup_host((host, port)))
                .await
                .map_err(|_| PinnedHttpError::Timeout)?
                .map_err(|_| PinnedHttpError::Dns)?
                .collect(),
        };
        let plaintext = url.scheme() == "http";
        let mut addresses = BTreeSet::new();
        for address in resolved {
            let admitted = self.private_access.admits(address.ip());
            if !admitted && !is_public_network_ip(address.ip()) {
                return Err(PinnedHttpError::NonPublicAddress);
            }
            // Plaintext is for destinations the deployment named, never for a
            // public address that merely shares a policy with them.
            if plaintext && !admitted {
                return Err(PinnedHttpError::HttpsRequired);
            }
            addresses.insert(address);
        }
        if addresses.is_empty() {
            return Err(PinnedHttpError::Dns);
        }
        let addresses: Vec<SocketAddr> = addresses.into_iter().collect();
        reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(self.timeout)
            .resolve_to_addrs(host, &addresses)
            .build()
            .map_err(|_| PinnedHttpError::BuildClient)
    }

    fn validate_scheme(&self, url: &Url) -> Result<(), PinnedHttpError> {
        match url.scheme() {
            "https" => Ok(()),
            "http" if self.private_access != PrivateNetworkAccess::None => Ok(()),
            "http" => Err(PinnedHttpError::HttpsRequired),
            _ => Err(PinnedHttpError::InvalidUrl),
        }
    }

    async fn execute_oauth(
        &self,
        request: OAuthHttpRequest,
    ) -> Result<oauth2::HttpResponse, PinnedHttpError> {
        let OAuthHttpRequest {
            request,
            redirect_policy,
            timeout,
            ..
        } = request;
        let mut request =
            reqwest::Request::try_from(request).map_err(|_| PinnedHttpError::InvalidRequest)?;
        let deadline = timeout.unwrap_or(self.timeout).min(self.timeout);

        tokio::time::timeout(deadline, async {
            let mut redirects = 0usize;
            loop {
                let url = request.url().clone();
                let client = self.client_for_url(&url).await?;
                let redirect_template = request.try_clone();
                let response = client.execute(request).await.map_err(|error| {
                    if error.is_timeout() {
                        PinnedHttpError::Timeout
                    } else {
                        PinnedHttpError::Request
                    }
                })?;

                if redirect_policy == OAuthHttpRedirectPolicy::Follow
                    && response.status().is_redirection()
                {
                    if redirects >= self.max_redirects {
                        return Err(PinnedHttpError::TooManyRedirects);
                    }
                    let location = response
                        .headers()
                        .get(reqwest::header::LOCATION)
                        .and_then(|value| value.to_str().ok())
                        .ok_or(PinnedHttpError::InvalidRedirect)?;
                    let next_url = url
                        .join(location)
                        .map_err(|_| PinnedHttpError::InvalidRedirect)?;
                    self.validate_scheme(&next_url)?;
                    let mut next = redirect_template.ok_or(PinnedHttpError::InvalidRedirect)?;
                    apply_redirect_method(response.status(), &mut next);
                    if !same_origin(&url, &next_url) {
                        next.headers_mut().remove(reqwest::header::AUTHORIZATION);
                        next.headers_mut().remove(reqwest::header::COOKIE);
                    }
                    *next.url_mut() = next_url;
                    request = next;
                    redirects += 1;
                    continue;
                }

                return self.convert_response(response).await;
            }
        })
        .await
        .map_err(|_| PinnedHttpError::Timeout)?
    }

    async fn convert_response(
        &self,
        response: reqwest::Response,
    ) -> Result<oauth2::HttpResponse, PinnedHttpError> {
        let header_bytes = response
            .headers()
            .iter()
            .try_fold(0usize, |total, (name, value)| {
                total
                    .checked_add(name.as_str().len())
                    .and_then(|total| total.checked_add(value.as_bytes().len()))
            });
        if header_bytes.is_none_or(|bytes| bytes > self.max_header_bytes) {
            return Err(PinnedHttpError::HeadersTooLarge);
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.max_body_bytes as u64)
        {
            return Err(PinnedHttpError::BodyTooLarge);
        }

        let status = response.status();
        let version = response.version();
        let headers = response.headers().clone();
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| PinnedHttpError::Request)?;
            if chunk.len() > self.max_body_bytes.saturating_sub(body.len()) {
                return Err(PinnedHttpError::BodyTooLarge);
            }
            body.extend_from_slice(&chunk);
        }

        let mut builder = oauth2::http::Response::builder()
            .status(status)
            .version(version);
        for (name, value) in &headers {
            builder = builder.header(name, value);
        }
        builder
            .body(body)
            .map_err(|_| PinnedHttpError::InvalidResponse)
    }
}

impl OAuthHttpClient for PinnedHttpPolicy {
    fn execute(&self, request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
        Box::pin(async move {
            self.execute_oauth(request)
                .await
                .map_err(|error| Box::new(error) as OAuthHttpClientError)
        })
    }
}

fn apply_redirect_method(status: StatusCode, request: &mut reqwest::Request) {
    let switch_to_get = status == StatusCode::SEE_OTHER
        || ((status == StatusCode::MOVED_PERMANENTLY || status == StatusCode::FOUND)
            && request.method() == Method::POST);
    if switch_to_get {
        *request.method_mut() = Method::GET;
        *request.body_mut() = None;
        request
            .headers_mut()
            .remove(reqwest::header::CONTENT_LENGTH);
        request.headers_mut().remove(reqwest::header::CONTENT_TYPE);
    }
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left
            .host_str()
            .zip(right.host_str())
            .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
        && left.port_or_known_default() == right.port_or_known_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PinnedHttpError {
    #[error("outbound URL is invalid")]
    InvalidUrl,
    #[error("outbound HTTP requires HTTPS")]
    HttpsRequired,
    #[error("outbound hostname resolution failed")]
    Dns,
    #[error("outbound hostname resolved to a forbidden address")]
    NonPublicAddress,
    #[error("outbound HTTP client construction failed")]
    BuildClient,
    #[error("outbound HTTP request is invalid")]
    InvalidRequest,
    #[error("outbound HTTP request failed")]
    Request,
    #[error("outbound HTTP request timed out")]
    Timeout,
    #[error("outbound HTTP redirect is invalid")]
    InvalidRedirect,
    #[error("outbound HTTP exceeded the redirect limit")]
    TooManyRedirects,
    #[error("outbound HTTP response headers exceeded the limit")]
    HeadersTooLarge,
    #[error("outbound HTTP response body exceeded the limit")]
    BodyTooLarge,
    #[error("outbound HTTP response is invalid")]
    InvalidResponse,
}

pub fn is_public_network_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => is_public_ipv6(ip),
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        || octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 192 && octets[1] == 88 && octets[2] == 99)
        || (octets[0] == 198 && (18..=19).contains(&octets[1]))
        || octets[0] >= 240)
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(ipv4) = ip.to_ipv4_mapped() {
        return is_public_ipv4(ipv4);
    }
    let segments = ip.segments();
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x0064 && segments[1] == 0xff9b && segments[2] == 0x0001)
        || (segments[0] == 0x0100 && segments[1] == 0 && segments[2] == 0 && segments[3] == 0)
        || (segments[0] == 0x2001 && segments[1] <= 0x01ff)
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || segments[0] == 0x2002)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_address_policy_rejects_special_ranges() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "192.0.2.1",
            "100.64.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
        ] {
            assert!(!is_public_network_ip(address.parse().unwrap()), "{address}");
        }
        assert!(is_public_network_ip("8.8.8.8".parse().unwrap()));
        assert!(is_public_network_ip(
            "2606:4700:4700::1111".parse().unwrap()
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn public_policy_rejects_loopback_before_connecting() {
        let url = Url::parse("https://127.0.0.1/oauth").unwrap();
        assert_eq!(
            PinnedHttpPolicy::public_only()
                .client_for_url(&url)
                .await
                .unwrap_err(),
            PinnedHttpError::NonPublicAddress
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn development_policy_allows_loopback_http() {
        let url = Url::parse("http://127.0.0.1:9/oauth").unwrap();
        PinnedHttpPolicy::allowing_private_networks()
            .client_for_url(&url)
            .await
            .expect("build pinned loopback client");
    }

    fn within(networks: &[&str]) -> PinnedHttpPolicy {
        let networks: Vec<IpNet> = networks
            .iter()
            .map(|network| network.parse().expect("network"))
            .collect();
        PinnedHttpPolicy::public_only()
            .with_private_access(PrivateNetworkAccess::Networks(networks.into()))
    }

    async fn verdict(policy: &PinnedHttpPolicy, url: &str) -> Result<(), PinnedHttpError> {
        policy
            .client_for_url(&Url::parse(url).unwrap())
            .await
            .map(|_| ())
    }

    /// `localhost` resolves to loopback on every host, which makes it a
    /// hostname whose resolution the test can rely on without the network.
    #[tokio::test(flavor = "current_thread")]
    async fn listed_network_admits_a_hostname_by_its_resolved_address() {
        let policy = within(&["127.0.0.0/8", "::1/128"]);
        assert_eq!(verdict(&policy, "https://localhost:9/mcp").await, Ok(()));
        assert_eq!(verdict(&policy, "http://localhost:9/mcp").await, Ok(()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unlisted_private_address_stays_forbidden_beside_a_listed_network() {
        let policy = within(&["10.0.0.0/8"]);
        for url in [
            "https://localhost:9/mcp",
            "https://127.0.0.1:9/mcp",
            "https://169.254.169.254/latest",
            "https://192.168.1.1/mcp",
            "https://[::1]:9/mcp",
        ] {
            assert_eq!(
                verdict(&policy, url).await,
                Err(PinnedHttpError::NonPublicAddress),
                "{url}"
            );
        }
        assert_eq!(verdict(&policy, "https://10.103.178.244/mcp").await, Ok(()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn listed_network_matches_ipv6_literals_and_mapped_ipv4() {
        let policy = within(&["fd00::/8", "10.0.0.0/8"]);
        assert_eq!(verdict(&policy, "http://[fd00::10]:9/mcp").await, Ok(()));
        assert_eq!(
            verdict(&policy, "http://[::ffff:10.1.2.3]:9/mcp").await,
            Ok(())
        );
        assert_eq!(
            verdict(&policy, "https://[fe80::1]:9/mcp").await,
            Err(PinnedHttpError::NonPublicAddress)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plaintext_reaches_only_listed_networks() {
        let policy = within(&["10.0.0.0/8"]);
        assert_eq!(verdict(&policy, "http://10.1.2.3/mcp").await, Ok(()));
        assert_eq!(
            verdict(&policy, "http://8.8.8.8/mcp").await,
            Err(PinnedHttpError::HttpsRequired)
        );
        assert_eq!(verdict(&policy, "https://8.8.8.8/mcp").await, Ok(()));
        assert_eq!(
            verdict(&PinnedHttpPolicy::public_only(), "http://8.8.8.8/mcp").await,
            Err(PinnedHttpError::HttpsRequired)
        );
    }
}
