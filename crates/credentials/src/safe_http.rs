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

/// The deployment's list of private destinations: exact hostnames and address
/// ranges, as one comma-separated setting spells them.
///
/// The list is the only thing that widens a pinned client beyond public
/// HTTPS, so every outbound client that honours it reaches the same places.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrivateNetworkList {
    hosts: Arc<[String]>,
    networks: Arc<[IpNet]>,
}

/// An entry that is written as a range but is not one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid CIDR in private network list: {item}")]
pub struct InvalidPrivateNetwork {
    pub item: String,
}

impl PrivateNetworkList {
    /// Parse comma-separated entries. An entry that parses as a CIDR range is
    /// a network; anything else without a `/` is an exact hostname (an IP
    /// literal without a prefix length is a hostname that names itself).
    pub fn parse(value: &str) -> Result<Self, InvalidPrivateNetwork> {
        let mut hosts = Vec::new();
        let mut networks = Vec::new();
        for item in value
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            if let Ok(network) = item.parse::<IpNet>() {
                networks.push(network);
            } else if item.contains('/') {
                return Err(InvalidPrivateNetwork {
                    item: item.to_owned(),
                });
            } else {
                hosts.push(item.to_ascii_lowercase());
            }
        }
        Ok(Self {
            hosts: hosts.into(),
            networks: networks.into(),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty() && self.networks.is_empty()
    }

    /// What a request to `url` may reach beyond public addresses.
    ///
    /// A listed host is trusted wherever it resolves. Any other host is held
    /// to the listed networks, and that is decided on its resolved addresses
    /// by the client that pins them: only there is a hostname's destination
    /// known, and known to be the one the connection uses.
    pub fn access_for(&self, url: &Url) -> PrivateNetworkAccess {
        let Some(host) = url.host_str() else {
            return PrivateNetworkAccess::None;
        };
        // A URL writes an IPv6 literal in brackets; the list does not.
        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_ascii_lowercase();
        if self.hosts.contains(&host) {
            return PrivateNetworkAccess::Any;
        }
        if self.networks.is_empty() {
            PrivateNetworkAccess::None
        } else {
            PrivateNetworkAccess::Networks(self.networks.clone())
        }
    }
}

/// How a pinned client decides private access for the URL in front of it.
#[derive(Clone, Debug)]
enum PrivateAccessRule {
    /// One decision the caller made for every URL, redirects included.
    Fixed(PrivateNetworkAccess),
    /// Decided again for each URL from the deployment's list. A client that
    /// talks to several hosts in one exchange needs this: an authorization
    /// server is a different host from the resource it protects, and listing
    /// one must not open the other.
    Listed(PrivateNetworkList),
}

#[derive(Clone, Debug)]
pub struct PinnedHttpPolicy {
    private_access: PrivateAccessRule,
    timeout: Duration,
    max_body_bytes: usize,
    max_header_bytes: usize,
    max_redirects: usize,
}

impl PinnedHttpPolicy {
    pub fn public_only() -> Self {
        Self {
            private_access: PrivateAccessRule::Fixed(PrivateNetworkAccess::None),
            timeout: Duration::from_secs(30),
            max_body_bytes: DEFAULT_OAUTH_HTTP_MAX_BODY_BYTES,
            max_header_bytes: DEFAULT_OAUTH_HTTP_MAX_HEADER_BYTES,
            max_redirects: DEFAULT_OAUTH_HTTP_MAX_REDIRECTS,
        }
    }

    /// Apply one private-access decision to every URL this client reaches.
    pub fn with_private_access(mut self, private_access: PrivateNetworkAccess) -> Self {
        self.private_access = PrivateAccessRule::Fixed(private_access);
        self
    }

    /// Decide private access per URL, redirect hops included, from the
    /// deployment's list.
    pub fn with_private_networks(mut self, private_networks: PrivateNetworkList) -> Self {
        self.private_access = PrivateAccessRule::Listed(private_networks);
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

    fn private_access_for(&self, url: &Url) -> PrivateNetworkAccess {
        match &self.private_access {
            PrivateAccessRule::Fixed(access) => access.clone(),
            PrivateAccessRule::Listed(list) => list.access_for(url),
        }
    }

    pub async fn client_for_url(&self, url: &Url) -> Result<reqwest::Client, PinnedHttpError> {
        let private_access = self.private_access_for(url);
        validate_scheme(url, &private_access)?;
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
            let admitted = private_access.admits(address.ip());
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
                    validate_scheme(&next_url, &self.private_access_for(&next_url))?;
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

/// Plaintext is considered only where the URL has some private access; whether
/// it is then allowed is decided per resolved address.
fn validate_scheme(
    url: &Url,
    private_access: &PrivateNetworkAccess,
) -> Result<(), PinnedHttpError> {
    match url.scheme() {
        "https" => Ok(()),
        "http" if *private_access != PrivateNetworkAccess::None => Ok(()),
        "http" => Err(PinnedHttpError::HttpsRequired),
        _ => Err(PinnedHttpError::InvalidUrl),
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
    async fn unrestricted_access_allows_loopback_http() {
        let url = Url::parse("http://127.0.0.1:9/oauth").unwrap();
        PinnedHttpPolicy::public_only()
            .with_private_access(PrivateNetworkAccess::Any)
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

    fn listed(entries: &str) -> PinnedHttpPolicy {
        PinnedHttpPolicy::public_only()
            .with_private_networks(PrivateNetworkList::parse(entries).expect("list"))
    }

    #[test]
    fn list_separates_hostnames_from_ranges_and_rejects_malformed_ranges() {
        let list =
            PrivateNetworkList::parse(" MCP.internal , ::1,10.20.0.0/16,,fd00::/8 ").expect("list");
        assert!(!list.is_empty());
        let networks = PrivateNetworkAccess::Networks(
            vec![
                "10.20.0.0/16".parse::<IpNet>().unwrap(),
                "fd00::/8".parse::<IpNet>().unwrap(),
            ]
            .into(),
        );
        let access = |url: &str| list.access_for(&Url::parse(url).unwrap());
        assert_eq!(
            access("https://mcp.INTERNAL/mcp"),
            PrivateNetworkAccess::Any
        );
        assert_eq!(access("http://[::1]:9/mcp"), PrivateNetworkAccess::Any);
        assert_eq!(access("https://login.internal/token"), networks);
        assert_eq!(access("http://10.20.4.8/mcp"), networks);

        assert_eq!(
            PrivateNetworkList::parse("10.0.0.0/40"),
            Err(InvalidPrivateNetwork {
                item: "10.0.0.0/40".to_owned()
            })
        );
        let empty = PrivateNetworkList::parse("").expect("empty list");
        assert!(empty.is_empty());
        assert_eq!(
            empty.access_for(&Url::parse("http://localhost/mcp").unwrap()),
            PrivateNetworkAccess::None
        );
    }

    /// One exchange reaches several hosts, so listing one of them must not
    /// open another that resolves to the same private address.
    #[tokio::test(flavor = "current_thread")]
    async fn listed_policy_decides_private_access_for_each_url() {
        let policy = listed("localhost");
        assert_eq!(verdict(&policy, "http://localhost:9/token").await, Ok(()));
        assert_eq!(verdict(&policy, "https://localhost:9/token").await, Ok(()));
        assert_eq!(
            verdict(&policy, "https://127.0.0.1:9/token").await,
            Err(PinnedHttpError::NonPublicAddress)
        );
        assert_eq!(
            verdict(&policy, "http://127.0.0.1:9/token").await,
            Err(PinnedHttpError::HttpsRequired)
        );
        assert_eq!(verdict(&policy, "https://8.8.8.8/token").await, Ok(()));
        assert_eq!(
            verdict(&policy, "http://8.8.8.8/token").await,
            Err(PinnedHttpError::HttpsRequired)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn listed_policy_holds_unlisted_hosts_to_the_listed_ranges() {
        let policy = listed("mcp.internal,10.0.0.0/8");
        assert_eq!(verdict(&policy, "http://10.1.2.3/register").await, Ok(()));
        assert_eq!(verdict(&policy, "https://10.1.2.3/register").await, Ok(()));
        assert_eq!(
            verdict(&policy, "https://192.168.1.1/register").await,
            Err(PinnedHttpError::NonPublicAddress)
        );
        assert_eq!(
            verdict(&policy, "https://169.254.169.254/latest").await,
            Err(PinnedHttpError::NonPublicAddress)
        );
        // Plaintext toward a public address stays refused even though the
        // list makes plaintext possible for the listed range.
        assert_eq!(
            verdict(&policy, "http://8.8.8.8/register").await,
            Err(PinnedHttpError::HttpsRequired)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn empty_list_is_public_https_only() {
        let policy = listed("");
        assert_eq!(
            verdict(&policy, "https://127.0.0.1/token").await,
            Err(PinnedHttpError::NonPublicAddress)
        );
        assert_eq!(
            verdict(&policy, "http://localhost/token").await,
            Err(PinnedHttpError::HttpsRequired)
        );
        assert_eq!(verdict(&policy, "https://8.8.8.8/token").await, Ok(()));
    }
}
