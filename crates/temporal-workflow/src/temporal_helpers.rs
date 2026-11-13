use std::{
    ffi::OsString,
    fmt, fs, io,
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use temporalio_client::{
    Client, ClientOptions, ClientTlsOptions, Connection, ConnectionOptions, TlsOptions,
};
use url::Url;

use crate::config::{DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET};

pub const TEMPORAL_ADDRESS_ENV: &str = "TEMPORAL_ADDRESS";
pub const TEMPORAL_NAMESPACE_ENV: &str = "TEMPORAL_NAMESPACE";
pub const TEMPORAL_TLS_ENV: &str = "TEMPORAL_TLS";
pub const TEMPORAL_TLS_SERVER_CA_FILE_ENV: &str = "TEMPORAL_TLS_SERVER_CA_FILE";
pub const TEMPORAL_TLS_SERVER_NAME_ENV: &str = "TEMPORAL_TLS_SERVER_NAME";
pub const TEMPORAL_TLS_CLIENT_CERT_FILE_ENV: &str = "TEMPORAL_TLS_CLIENT_CERT_FILE";
pub const TEMPORAL_TLS_CLIENT_KEY_FILE_ENV: &str = "TEMPORAL_TLS_CLIENT_KEY_FILE";
pub const TEMPORAL_API_KEY_ENV: &str = "TEMPORAL_API_KEY";
pub const TEMPORAL_API_KEY_FILE_ENV: &str = "TEMPORAL_API_KEY_FILE";

/// Port of a Temporal frontend when the address names a bare host.
const DEFAULT_TEMPORAL_PORT: u16 = 7233;

/// Temporal connection settings exactly as the operator wrote them. Nothing
/// is validated and no file is read until [`Self::resolve`] and
/// [`connect_temporal`].
///
/// A blank value counts as unset, so a deployment template may leave a
/// variable empty. The type has no `Debug` because it can hold the API key.
#[derive(Clone, Default)]
pub struct TemporalConnectionSettings {
    /// `host:port`, or an `http://` or `https://` URL. Without a port, a
    /// bare host uses 7233 and a URL the default port of its scheme.
    pub address: Option<String>,
    pub namespace: Option<String>,
    /// `true` or `false`. Unset lets the other settings decide.
    pub tls: Option<String>,
    pub tls_server_ca_file: Option<String>,
    pub tls_server_name: Option<String>,
    pub tls_client_cert_file: Option<String>,
    pub tls_client_key_file: Option<String>,
    pub api_key: Option<String>,
    pub api_key_file: Option<String>,
}

impl TemporalConnectionSettings {
    /// Read the nine `TEMPORAL_*` connection variables from the process
    /// environment.
    pub fn from_env() -> Result<Self, TemporalConfigError> {
        Self::from_lookup(std::env::var_os)
    }

    /// A value that is not valid Unicode is refused instead of ignored: a
    /// dropped TLS or key setting would silently weaken the connection.
    fn from_lookup(
        lookup: impl Fn(&'static str) -> Option<OsString>,
    ) -> Result<Self, TemporalConfigError> {
        let read = |variable: &'static str| match lookup(variable) {
            None => Ok(None),
            Some(value) => value
                .into_string()
                .map(Some)
                .map_err(|_| TemporalConfigError::NotUnicode { variable }),
        };
        Ok(Self {
            address: read(TEMPORAL_ADDRESS_ENV)?,
            namespace: read(TEMPORAL_NAMESPACE_ENV)?,
            tls: read(TEMPORAL_TLS_ENV)?,
            tls_server_ca_file: read(TEMPORAL_TLS_SERVER_CA_FILE_ENV)?,
            tls_server_name: read(TEMPORAL_TLS_SERVER_NAME_ENV)?,
            tls_client_cert_file: read(TEMPORAL_TLS_CLIENT_CERT_FILE_ENV)?,
            tls_client_key_file: read(TEMPORAL_TLS_CLIENT_KEY_FILE_ENV)?,
            api_key: read(TEMPORAL_API_KEY_ENV)?,
            api_key_file: read(TEMPORAL_API_KEY_FILE_ENV)?,
        })
    }

    /// Check the settings against each other and decide whether the
    /// connection uses TLS. No file is touched here.
    ///
    /// TLS is on when `tls` says `true`. When `tls` is unset it is on if any
    /// TLS setting or an API key is present, or the address is an `https://`
    /// URL: an API key is never sent in the clear unless `tls` says `false`.
    pub fn resolve(self) -> Result<TemporalConnectionConfig, TemporalConfigError> {
        let address = present(self.address).unwrap_or_else(|| DEFAULT_TEMPORAL_TARGET.to_owned());
        let namespace =
            present(self.namespace).unwrap_or_else(|| DEFAULT_TEMPORAL_NAMESPACE.to_owned());
        let explicit_tls = present(self.tls)
            .map(|value| parse_tls_switch(&value))
            .transpose()?;
        let server_ca_file = present(self.tls_server_ca_file);
        let server_name = present(self.tls_server_name);
        let client_cert_file = present(self.tls_client_cert_file);
        let client_key_file = present(self.tls_client_key_file);
        let api_key = present(self.api_key);
        let api_key_file = present(self.api_key_file);

        let api_key = match (api_key, api_key_file) {
            (Some(_), Some(_)) => return Err(TemporalConfigError::ApiKeyGivenTwice),
            (Some(value), None) => Some(TemporalApiKey::Value(validated_api_key(
                TEMPORAL_API_KEY_ENV,
                &value,
            )?)),
            (None, Some(path)) => Some(TemporalApiKey::File(PathBuf::from(path))),
            (None, None) => None,
        };
        let client_certificate = match (client_cert_file, client_key_file) {
            (Some(cert_file), Some(key_file)) => Some(TemporalClientCertificate {
                cert_file: PathBuf::from(cert_file),
                key_file: PathBuf::from(key_file),
            }),
            (Some(_), None) => {
                return Err(TemporalConfigError::IncompleteClientCertificate {
                    present: TEMPORAL_TLS_CLIENT_CERT_FILE_ENV,
                    missing: TEMPORAL_TLS_CLIENT_KEY_FILE_ENV,
                });
            }
            (None, Some(_)) => {
                return Err(TemporalConfigError::IncompleteClientCertificate {
                    present: TEMPORAL_TLS_CLIENT_KEY_FILE_ENV,
                    missing: TEMPORAL_TLS_CLIENT_CERT_FILE_ENV,
                });
            }
            (None, None) => None,
        };
        if let Some(name) = server_name.as_deref() {
            validate_server_name(name)?;
        }

        let (scheme, authority) = split_address(&address)?;
        // The first setting that only makes sense over TLS. It both turns TLS
        // on when the switch is unset and names the conflict when it is off.
        let tls_setting = [
            (TEMPORAL_TLS_SERVER_CA_FILE_ENV, server_ca_file.is_some()),
            (TEMPORAL_TLS_SERVER_NAME_ENV, server_name.is_some()),
            (
                TEMPORAL_TLS_CLIENT_CERT_FILE_ENV,
                client_certificate.is_some(),
            ),
        ]
        .into_iter()
        .find_map(|(variable, set)| set.then_some(variable));
        let enabled_by = match explicit_tls {
            Some(false) => {
                if let Some(variable) = tls_setting {
                    return Err(TemporalConfigError::TlsDisabledWithSetting { variable });
                }
                if scheme == Some(AddressScheme::Https) {
                    return Err(TemporalConfigError::TlsDisabledWithHttpsAddress);
                }
                None
            }
            Some(true) => Some(TEMPORAL_TLS_ENV),
            None => tls_setting
                .or(match &api_key {
                    Some(TemporalApiKey::Value(_)) => Some(TEMPORAL_API_KEY_ENV),
                    Some(TemporalApiKey::File(_)) => Some(TEMPORAL_API_KEY_FILE_ENV),
                    None => None,
                })
                .or((scheme == Some(AddressScheme::Https)).then_some(TEMPORAL_ADDRESS_ENV)),
        };
        if let Some(enabled_by) = enabled_by
            && scheme == Some(AddressScheme::Http)
        {
            return Err(TemporalConfigError::TlsEnabledWithHttpAddress { enabled_by });
        }

        // The transport encrypts only an `https` target, whatever TLS options
        // it is handed, so the scheme always follows the decision made here.
        let target_scheme = if enabled_by.is_some() {
            "https"
        } else {
            "http"
        };
        let mut target = Url::parse(&format!("{target_scheme}://{authority}"))
            .ok()
            .filter(names_only_host_and_port)
            .ok_or(TemporalConfigError::InvalidAddress)?;
        // A bare host is not a URL, so it takes Temporal's port and not the
        // scheme's: otherwise its port would move with the TLS decision, and
        // differ from the connector host, whose client fills in the same port.
        if scheme.is_none() && !has_explicit_port(authority) {
            target
                .set_port(Some(DEFAULT_TEMPORAL_PORT))
                .map_err(|()| TemporalConfigError::InvalidAddress)?;
        }

        Ok(TemporalConnectionConfig {
            target,
            namespace,
            tls: enabled_by.map(|_| TemporalTlsConfig {
                server_ca_file: server_ca_file.map(PathBuf::from),
                server_name,
                client_certificate,
            }),
            api_key,
        })
    }
}

/// A validated way to reach one Temporal namespace: where, whether over TLS,
/// and how this process identifies itself. Certificates and a key file are
/// named by path here and read when the connection is made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporalConnectionConfig {
    target: Url,
    namespace: String,
    tls: Option<TemporalTlsConfig>,
    api_key: Option<TemporalApiKey>,
}

impl TemporalConnectionConfig {
    /// The connection the `TEMPORAL_*` environment variables describe. With
    /// none of them set this is an unencrypted, unauthenticated connection to
    /// the local default frontend and namespace.
    pub fn from_env() -> Result<Self, TemporalConfigError> {
        TemporalConnectionSettings::from_env()?.resolve()
    }

    /// The frontend URL. Its scheme is `https` exactly when TLS is on.
    pub fn target(&self) -> &Url {
        &self.target
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// TLS settings, or `None` for an unencrypted connection.
    pub fn tls(&self) -> Option<&TemporalTlsConfig> {
        self.tls.as_ref()
    }

    pub fn uses_client_certificate(&self) -> bool {
        self.tls
            .as_ref()
            .is_some_and(|tls| tls.client_certificate.is_some())
    }

    pub fn uses_api_key(&self) -> bool {
        self.api_key.is_some()
    }

    /// Read the named files and build the client's options. The result holds
    /// the private key and the API key and its `Debug` prints the API key, so
    /// it must never be logged or formatted.
    fn connection_options(&self) -> Result<ConnectionOptions, TemporalConfigError> {
        let tls = self.tls.as_ref().map(TemporalTlsConfig::load).transpose()?;
        let api_key = self
            .api_key
            .as_ref()
            .map(TemporalApiKey::load)
            .transpose()?;
        Ok(ConnectionOptions::new(self.target.clone())
            .maybe_tls_options(tls)
            .maybe_api_key(api_key)
            .build())
    }
}

/// How the server is verified and, optionally, the certificate this process
/// presents. Without a CA file the server certificate must chain to a root
/// the operating system trusts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporalTlsConfig {
    pub server_ca_file: Option<PathBuf>,
    /// Name the server certificate is checked against instead of the host in
    /// the address. It is also sent as the request authority.
    pub server_name: Option<String>,
    pub client_certificate: Option<TemporalClientCertificate>,
}

impl TemporalTlsConfig {
    fn load(&self) -> Result<TlsOptions, TemporalConfigError> {
        let server_root_ca_cert = self
            .server_ca_file
            .as_deref()
            .map(|path| read_pem(TEMPORAL_TLS_SERVER_CA_FILE_ENV, path, PemKind::Certificate))
            .transpose()?;
        let client_tls_options = self
            .client_certificate
            .as_ref()
            .map(|identity| {
                let client_cert = read_pem(
                    TEMPORAL_TLS_CLIENT_CERT_FILE_ENV,
                    &identity.cert_file,
                    PemKind::Certificate,
                )?;
                let client_private_key = read_pem(
                    TEMPORAL_TLS_CLIENT_KEY_FILE_ENV,
                    &identity.key_file,
                    PemKind::PrivateKey,
                )?;
                Ok(ClientTlsOptions::builder()
                    .client_cert(client_cert)
                    .client_private_key(client_private_key)
                    .build())
            })
            .transpose()?;
        Ok(TlsOptions::builder()
            .maybe_server_root_ca_cert(server_root_ca_cert)
            .maybe_domain(self.server_name.clone())
            .maybe_client_tls_options(client_tls_options)
            .build())
    }
}

/// PEM files of the certificate (chain) and private key presented to the
/// server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporalClientCertificate {
    pub cert_file: PathBuf,
    pub key_file: PathBuf,
}

#[derive(Clone, PartialEq, Eq)]
enum TemporalApiKey {
    Value(String),
    File(PathBuf),
}

impl TemporalApiKey {
    fn load(&self) -> Result<String, TemporalConfigError> {
        match self {
            Self::Value(value) => Ok(value.clone()),
            Self::File(path) => {
                let contents = fs::read_to_string(path).map_err(|source| {
                    TemporalConfigError::UnreadableFile {
                        variable: TEMPORAL_API_KEY_FILE_ENV,
                        path: path.clone(),
                        source,
                    }
                })?;
                validated_api_key(TEMPORAL_API_KEY_FILE_ENV, &contents)
            }
        }
    }
}

impl fmt::Debug for TemporalApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Value(_) => formatter.write_str("Value(<redacted>)"),
            Self::File(path) => formatter.debug_tuple("File").field(path).finish(),
        }
    }
}

/// A Temporal connection setting that cannot be used as written.
#[derive(Debug)]
pub enum TemporalConfigError {
    NotUnicode {
        variable: &'static str,
    },
    InvalidTlsSwitch,
    /// The address is not `host:port` or an `http://`/`https://` URL of a
    /// host and port alone.
    InvalidAddress,
    InvalidServerName,
    TlsDisabledWithSetting {
        variable: &'static str,
    },
    TlsDisabledWithHttpsAddress,
    TlsEnabledWithHttpAddress {
        enabled_by: &'static str,
    },
    IncompleteClientCertificate {
        present: &'static str,
        missing: &'static str,
    },
    ApiKeyGivenTwice,
    /// The key is empty or holds a character an HTTP header cannot carry.
    InvalidApiKey {
        variable: &'static str,
    },
    UnreadableFile {
        variable: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    /// The file holds no PEM block of the kind the setting needs.
    UnexpectedFileContents {
        variable: &'static str,
        path: PathBuf,
        expected: &'static str,
    },
}

impl fmt::Display for TemporalConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotUnicode { variable } => write!(formatter, "{variable} is not valid Unicode"),
            Self::InvalidTlsSwitch => write!(formatter, "{TEMPORAL_TLS_ENV} must be true or false"),
            Self::InvalidAddress => write!(
                formatter,
                "{TEMPORAL_ADDRESS_ENV} must be host:port or an http:// or https:// URL, \
                 without user information, path or query"
            ),
            Self::InvalidServerName => write!(
                formatter,
                "{TEMPORAL_TLS_SERVER_NAME_ENV} must be a bare host name, without a scheme, port or path"
            ),
            Self::TlsDisabledWithSetting { variable } => write!(
                formatter,
                "{TEMPORAL_TLS_ENV}=false contradicts {variable}; unset one of them"
            ),
            Self::TlsDisabledWithHttpsAddress => write!(
                formatter,
                "{TEMPORAL_TLS_ENV}=false contradicts the https:// scheme of {TEMPORAL_ADDRESS_ENV}"
            ),
            Self::TlsEnabledWithHttpAddress { enabled_by } => write!(
                formatter,
                "{enabled_by} turns TLS on, but {TEMPORAL_ADDRESS_ENV} is an http:// URL; \
                 write the address as host:port or https://, or set {TEMPORAL_TLS_ENV}=false \
                 for an unencrypted connection"
            ),
            Self::IncompleteClientCertificate { present, missing } => write!(
                formatter,
                "{present} is set without {missing}; a client certificate needs both files"
            ),
            Self::ApiKeyGivenTwice => write!(
                formatter,
                "set only one of {TEMPORAL_API_KEY_ENV} and {TEMPORAL_API_KEY_FILE_ENV}"
            ),
            Self::InvalidApiKey { variable } => write!(
                formatter,
                "{variable} must hold a non-empty key of printable ASCII characters without spaces"
            ),
            Self::UnreadableFile {
                variable,
                path,
                source,
            } => write!(
                formatter,
                "{variable}: cannot read {}: {source}",
                path.display()
            ),
            Self::UnexpectedFileContents {
                variable,
                path,
                expected,
            } => write!(
                formatter,
                "{variable}: {} does not hold {expected}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for TemporalConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UnreadableFile { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Connect to Temporal as `config` describes. TLS and the API key are set on
/// the client explicitly; certificate, key and API key files are read here,
/// once, so a rotated file takes effect at the next process start.
pub async fn connect_temporal(config: &TemporalConnectionConfig) -> anyhow::Result<Client> {
    let options = config.connection_options()?;
    let connection = Connection::connect(options).await.with_context(|| {
        format!(
            "connect to the Temporal frontend at {}",
            config.target.origin().ascii_serialization()
        )
    })?;
    Ok(Client::new(
        connection,
        ClientOptions::new(config.namespace.clone()).build(),
    )?)
}

fn present(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn parse_tls_switch(value: &str) -> Result<bool, TemporalConfigError> {
    if value.eq_ignore_ascii_case("true") {
        Ok(true)
    } else if value.eq_ignore_ascii_case("false") {
        Ok(false)
    } else {
        Err(TemporalConfigError::InvalidTlsSwitch)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AddressScheme {
    Http,
    Https,
}

/// Split an optional `http`/`https` scheme from the rest of the address.
fn split_address(address: &str) -> Result<(Option<AddressScheme>, &str), TemporalConfigError> {
    let Some((scheme, authority)) = address.split_once("://") else {
        return Ok((None, address));
    };
    let scheme = if scheme.eq_ignore_ascii_case("http") {
        AddressScheme::Http
    } else if scheme.eq_ignore_ascii_case("https") {
        AddressScheme::Https
    } else {
        return Err(TemporalConfigError::InvalidAddress);
    };
    Ok((Some(scheme), authority))
}

/// The address names a server and nothing else. User information, a path or
/// a query has no meaning for the connection, and the connector host refuses
/// the same forms, so one value is valid in both processes or in neither.
fn names_only_host_and_port(url: &Url) -> bool {
    url.host().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
}

/// Whether the text after the host, which may be a bracketed IPv6 literal,
/// carries a port.
fn has_explicit_port(authority: &str) -> bool {
    let host_end = authority.rfind(']').map_or(0, |index| index + 1);
    authority[host_end..]
        .split_once(':')
        .is_some_and(|(_, port)| !port.trim_end_matches('/').is_empty())
}

/// The override becomes both the verified certificate name and the request
/// authority, so anything but a bare host would fail later with an opaque
/// transport error.
fn validate_server_name(name: &str) -> Result<(), TemporalConfigError> {
    let bare_host = Url::parse(&format!("https://{name}/"))
        .ok()
        .is_some_and(|url| {
            url.port().is_none()
                && url.username().is_empty()
                && url.password().is_none()
                && url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none()
                && url
                    .host_str()
                    .is_some_and(|host| host.eq_ignore_ascii_case(name))
        });
    if bare_host {
        Ok(())
    } else {
        Err(TemporalConfigError::InvalidServerName)
    }
}

/// The client sends the key as a bearer header and silently drops a header
/// it cannot encode, which would turn a mistyped key into an unauthenticated
/// connection. Refuse such a key up front.
fn validated_api_key(variable: &'static str, raw: &str) -> Result<String, TemporalConfigError> {
    let key = raw.trim();
    if key.is_empty() || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(TemporalConfigError::InvalidApiKey { variable });
    }
    Ok(key.to_owned())
}

#[derive(Clone, Copy)]
enum PemKind {
    Certificate,
    PrivateKey,
}

/// Read a PEM file and check that it holds the kind of block the setting
/// needs, which catches a swapped certificate and key before the handshake.
fn read_pem(
    variable: &'static str,
    path: &Path,
    kind: PemKind,
) -> Result<Vec<u8>, TemporalConfigError> {
    let bytes = fs::read(path).map_err(|source| TemporalConfigError::UnreadableFile {
        variable,
        path: path.to_owned(),
        source,
    })?;
    let text = String::from_utf8_lossy(&bytes);
    let (matches, expected) = match kind {
        PemKind::Certificate => (
            text.contains("-----BEGIN CERTIFICATE-----"),
            "a PEM certificate",
        ),
        PemKind::PrivateKey => (
            text.contains("-----BEGIN ")
                && text.contains("PRIVATE KEY-----")
                && !text.contains("ENCRYPTED"),
            "an unencrypted PEM private key",
        ),
    };
    if !matches {
        return Err(TemporalConfigError::UnexpectedFileContents {
            variable,
            path: path.to_owned(),
            expected,
        });
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    const CERTIFICATE_PEM: &str = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";
    const PRIVATE_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----\nMIIE\n-----END PRIVATE KEY-----\n";

    fn settings(values: &[(&'static str, &str)]) -> TemporalConnectionSettings {
        let values: Vec<(&'static str, String)> = values
            .iter()
            .map(|(name, value)| (*name, (*value).to_owned()))
            .collect();
        TemporalConnectionSettings::from_lookup(|name| {
            values
                .iter()
                .find(|(candidate, _)| *candidate == name)
                .map(|(_, value)| OsString::from(value))
        })
        .expect("unicode settings")
    }

    fn resolve(
        values: &[(&'static str, &str)],
    ) -> Result<TemporalConnectionConfig, TemporalConfigError> {
        settings(values).resolve()
    }

    fn write(directory: &tempfile::TempDir, name: &str, contents: &str) -> String {
        let path = directory.path().join(name);
        fs::write(&path, contents).expect("write fixture file");
        path.to_str().expect("unicode temporary path").to_owned()
    }

    #[test]
    fn nothing_set_is_the_unencrypted_local_default() {
        let config = resolve(&[]).expect("default connection");
        assert_eq!(config.target().as_str(), "http://localhost:7233/");
        assert_eq!(config.namespace(), DEFAULT_TEMPORAL_NAMESPACE);
        assert!(config.tls().is_none());
        assert!(!config.uses_api_key());
        assert!(!config.uses_client_certificate());

        let options = config.connection_options().expect("connection options");
        assert!(options.tls_options.is_none());
        assert!(options.api_key.is_none());
    }

    #[test]
    fn blank_values_count_as_unset() {
        let config = resolve(&[
            (TEMPORAL_ADDRESS_ENV, "  "),
            (TEMPORAL_NAMESPACE_ENV, ""),
            (TEMPORAL_TLS_ENV, ""),
            (TEMPORAL_TLS_SERVER_CA_FILE_ENV, " "),
            (TEMPORAL_TLS_CLIENT_CERT_FILE_ENV, ""),
            (TEMPORAL_API_KEY_ENV, ""),
            (TEMPORAL_API_KEY_FILE_ENV, ""),
        ])
        .expect("blank settings");
        assert_eq!(config, resolve(&[]).expect("default connection"));
    }

    #[test]
    fn a_bare_address_keeps_the_shared_host_port_form() {
        let config = resolve(&[
            (TEMPORAL_ADDRESS_ENV, "temporal.internal:7233"),
            (TEMPORAL_NAMESPACE_ENV, "lightspeed"),
        ])
        .expect("host:port address");
        assert_eq!(config.target().as_str(), "http://temporal.internal:7233/");
        assert_eq!(config.namespace(), "lightspeed");
        assert!(config.tls().is_none());
    }

    #[test]
    fn the_tls_switch_selects_the_scheme_of_a_bare_address() {
        let config = resolve(&[
            (TEMPORAL_ADDRESS_ENV, "temporal.internal:7233"),
            (TEMPORAL_TLS_ENV, "TRUE"),
        ])
        .expect("TLS connection");
        assert_eq!(config.target().as_str(), "https://temporal.internal:7233/");
        assert_eq!(
            config.tls(),
            Some(&TemporalTlsConfig {
                server_ca_file: None,
                server_name: None,
                client_certificate: None,
            })
        );
        // TLS options are handed to the client even with nothing to customize:
        // the address scheme alone is not what turns verification on.
        let options = config.connection_options().expect("connection options");
        assert!(options.tls_options.is_some());
    }

    #[test]
    fn an_https_address_turns_tls_on_with_explicit_options() {
        let config =
            resolve(&[(TEMPORAL_ADDRESS_ENV, "https://temporal.example.test:7233")]).expect("URL");
        assert_eq!(
            config.target().as_str(),
            "https://temporal.example.test:7233/"
        );
        assert!(config.tls().is_some());
        assert!(
            config
                .connection_options()
                .expect("connection options")
                .tls_options
                .is_some()
        );
    }

    #[test]
    fn an_http_address_stays_unencrypted() {
        let config =
            resolve(&[(TEMPORAL_ADDRESS_ENV, "http://temporal.internal:7233")]).expect("URL");
        assert_eq!(config.target().as_str(), "http://temporal.internal:7233/");
        assert!(config.tls().is_none());
    }

    #[test]
    fn a_bare_host_takes_the_temporal_port_and_a_url_the_port_of_its_scheme() {
        type Settings = &'static [(&'static str, &'static str)];
        let cases: [(Settings, &str, u16); 9] = [
            (&[(TEMPORAL_ADDRESS_ENV, "temporal.internal")], "http", 7233),
            // The port of a bare host does not move with the TLS decision.
            (
                &[
                    (TEMPORAL_ADDRESS_ENV, "temporal.internal"),
                    (TEMPORAL_TLS_ENV, "true"),
                ],
                "https",
                7233,
            ),
            (
                &[(TEMPORAL_ADDRESS_ENV, "temporal.internal/")],
                "http",
                7233,
            ),
            (&[(TEMPORAL_ADDRESS_ENV, "[::1]")], "http", 7233),
            (&[(TEMPORAL_ADDRESS_ENV, "[::1]:7244")], "http", 7244),
            // A port that was written is kept, also when it is the scheme's own.
            (
                &[(TEMPORAL_ADDRESS_ENV, "temporal.internal:80")],
                "http",
                80,
            ),
            (
                &[
                    (TEMPORAL_ADDRESS_ENV, "temporal.internal:443"),
                    (TEMPORAL_TLS_ENV, "true"),
                ],
                "https",
                443,
            ),
            (
                &[(TEMPORAL_ADDRESS_ENV, "http://temporal.internal")],
                "http",
                80,
            ),
            (
                &[(TEMPORAL_ADDRESS_ENV, "https://temporal.example.test/")],
                "https",
                443,
            ),
        ];
        for (values, scheme, port) in cases {
            let config = resolve(values).expect("address");
            assert_eq!(config.target().scheme(), scheme, "{values:?}");
            assert_eq!(
                config.target().port_or_known_default(),
                Some(port),
                "{values:?}"
            );
        }
    }

    #[test]
    fn any_tls_setting_turns_tls_on() {
        for (variable, value) in [
            (TEMPORAL_TLS_SERVER_CA_FILE_ENV, "/etc/temporal/ca.pem"),
            (TEMPORAL_TLS_SERVER_NAME_ENV, "frontend.temporal.internal"),
        ] {
            let config = resolve(&[(variable, value)]).expect("TLS setting");
            assert_eq!(config.target().scheme(), "https", "{variable}");
            assert!(config.tls().is_some(), "{variable}");
        }
        let config = resolve(&[
            (
                TEMPORAL_TLS_CLIENT_CERT_FILE_ENV,
                "/etc/temporal/client.pem",
            ),
            (TEMPORAL_TLS_CLIENT_KEY_FILE_ENV, "/etc/temporal/client.key"),
        ])
        .expect("client certificate");
        assert_eq!(config.target().scheme(), "https");
        assert!(config.uses_client_certificate());
    }

    #[test]
    fn an_api_key_is_not_sent_in_the_clear_unless_tls_is_switched_off() {
        let implied = resolve(&[(TEMPORAL_API_KEY_ENV, "key-1")]).expect("API key");
        assert_eq!(implied.target().scheme(), "https");
        assert!(implied.tls().is_some());
        assert!(implied.uses_api_key());

        let from_file =
            resolve(&[(TEMPORAL_API_KEY_FILE_ENV, "/run/secrets/temporal")]).expect("API key file");
        assert_eq!(from_file.target().scheme(), "https");

        let plain = resolve(&[(TEMPORAL_API_KEY_ENV, "key-1"), (TEMPORAL_TLS_ENV, "false")])
            .expect("explicitly unencrypted");
        assert_eq!(plain.target().scheme(), "http");
        assert!(plain.tls().is_none());
        assert!(plain.uses_api_key());
        assert_eq!(
            plain
                .connection_options()
                .expect("connection options")
                .api_key
                .as_deref(),
            Some("key-1")
        );
    }

    #[test]
    fn the_tls_switch_accepts_only_true_or_false() {
        for value in ["1", "yes", "on", "tls"] {
            assert!(
                matches!(
                    resolve(&[(TEMPORAL_TLS_ENV, value)]),
                    Err(TemporalConfigError::InvalidTlsSwitch)
                ),
                "{value}"
            );
        }
    }

    #[test]
    fn switching_tls_off_contradicts_every_tls_setting() {
        for (variable, extra) in [
            (TEMPORAL_TLS_SERVER_CA_FILE_ENV, None),
            (TEMPORAL_TLS_SERVER_NAME_ENV, None),
            (
                TEMPORAL_TLS_CLIENT_CERT_FILE_ENV,
                Some(TEMPORAL_TLS_CLIENT_KEY_FILE_ENV),
            ),
        ] {
            let mut values = vec![(TEMPORAL_TLS_ENV, "false"), (variable, "value")];
            if let Some(extra) = extra {
                values.push((extra, "value"));
            }
            assert!(
                matches!(
                    resolve(&values),
                    Err(TemporalConfigError::TlsDisabledWithSetting { variable: named })
                        if named == variable
                ),
                "{variable}"
            );
        }
        assert!(matches!(
            resolve(&[
                (TEMPORAL_TLS_ENV, "false"),
                (TEMPORAL_ADDRESS_ENV, "https://temporal.example.test:7233"),
            ]),
            Err(TemporalConfigError::TlsDisabledWithHttpsAddress)
        ));
    }

    #[test]
    fn tls_contradicts_an_http_address_and_names_what_turned_it_on() {
        let address = (TEMPORAL_ADDRESS_ENV, "http://temporal.internal:7233");
        for (setting, enabled_by) in [
            ((TEMPORAL_TLS_ENV, "true"), TEMPORAL_TLS_ENV),
            (
                (TEMPORAL_TLS_SERVER_NAME_ENV, "frontend.internal"),
                TEMPORAL_TLS_SERVER_NAME_ENV,
            ),
            ((TEMPORAL_API_KEY_ENV, "key-1"), TEMPORAL_API_KEY_ENV),
            (
                (TEMPORAL_API_KEY_FILE_ENV, "/run/secrets/temporal"),
                TEMPORAL_API_KEY_FILE_ENV,
            ),
        ] {
            assert!(
                matches!(
                    resolve(&[address, setting]),
                    Err(TemporalConfigError::TlsEnabledWithHttpAddress { enabled_by: named })
                        if named == enabled_by
                ),
                "{enabled_by}"
            );
        }
    }

    #[test]
    fn a_client_certificate_needs_both_files() {
        assert!(matches!(
            resolve(&[(
                TEMPORAL_TLS_CLIENT_CERT_FILE_ENV,
                "/etc/temporal/client.pem"
            )]),
            Err(TemporalConfigError::IncompleteClientCertificate {
                present: TEMPORAL_TLS_CLIENT_CERT_FILE_ENV,
                missing: TEMPORAL_TLS_CLIENT_KEY_FILE_ENV,
            })
        ));
        assert!(matches!(
            resolve(&[(TEMPORAL_TLS_CLIENT_KEY_FILE_ENV, "/etc/temporal/client.key")]),
            Err(TemporalConfigError::IncompleteClientCertificate {
                present: TEMPORAL_TLS_CLIENT_KEY_FILE_ENV,
                missing: TEMPORAL_TLS_CLIENT_CERT_FILE_ENV,
            })
        ));
    }

    #[test]
    fn the_api_key_has_one_source_and_must_fit_a_header() {
        assert!(matches!(
            resolve(&[
                (TEMPORAL_API_KEY_ENV, "key-1"),
                (TEMPORAL_API_KEY_FILE_ENV, "/run/secrets/temporal"),
            ]),
            Err(TemporalConfigError::ApiKeyGivenTwice)
        ));
        for key in ["two words", "line\nbreak", "sch\u{fc}ssel"] {
            assert!(
                matches!(
                    resolve(&[(TEMPORAL_API_KEY_ENV, key)]),
                    Err(TemporalConfigError::InvalidApiKey {
                        variable: TEMPORAL_API_KEY_ENV
                    })
                ),
                "{key:?}"
            );
        }
    }

    #[test]
    fn addresses_and_server_names_are_checked() {
        for address in [
            "grpc://temporal:7233",
            "https://",
            ":7233",
            "temporal:7233/path",
            "http://temporal:7233/path",
            "temporal:7233?tls=1",
            "https://temporal:7233#frontend",
            "https://user:secret@temporal:7233",
            "user@temporal:7233",
        ] {
            assert!(
                matches!(
                    resolve(&[(TEMPORAL_ADDRESS_ENV, address)]),
                    Err(TemporalConfigError::InvalidAddress)
                ),
                "{address}"
            );
        }
        for name in [
            "https://frontend.internal",
            "frontend.internal:7233",
            "frontend.internal/path",
            "two names",
        ] {
            assert!(
                matches!(
                    resolve(&[(TEMPORAL_TLS_SERVER_NAME_ENV, name)]),
                    Err(TemporalConfigError::InvalidServerName)
                ),
                "{name}"
            );
        }
        assert!(resolve(&[(TEMPORAL_TLS_SERVER_NAME_ENV, "Frontend.Internal")]).is_ok());
    }

    #[test]
    fn a_setting_that_is_not_unicode_is_refused_instead_of_ignored() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt as _;
            let result = TemporalConnectionSettings::from_lookup(|name| {
                (name == TEMPORAL_TLS_SERVER_CA_FILE_ENV)
                    .then(|| OsString::from_vec(vec![b'/', 0xff]))
            });
            assert!(matches!(
                result,
                Err(TemporalConfigError::NotUnicode {
                    variable: TEMPORAL_TLS_SERVER_CA_FILE_ENV
                })
            ));
        }
    }

    #[test]
    fn files_are_read_into_explicit_client_options() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let ca = write(&directory, "ca.pem", CERTIFICATE_PEM);
        let cert = write(&directory, "client.pem", CERTIFICATE_PEM);
        let key = write(&directory, "client.key", PRIVATE_KEY_PEM);
        let api_key = write(&directory, "api-key", "key-from-file\n");

        let config = resolve(&[
            (TEMPORAL_ADDRESS_ENV, "temporal.internal:7233"),
            (TEMPORAL_TLS_SERVER_CA_FILE_ENV, &ca),
            (TEMPORAL_TLS_SERVER_NAME_ENV, "frontend.temporal.internal"),
            (TEMPORAL_TLS_CLIENT_CERT_FILE_ENV, &cert),
            (TEMPORAL_TLS_CLIENT_KEY_FILE_ENV, &key),
            (TEMPORAL_API_KEY_FILE_ENV, &api_key),
        ])
        .expect("full TLS configuration");
        let options = config.connection_options().expect("connection options");

        assert_eq!(options.target.as_str(), "https://temporal.internal:7233/");
        assert_eq!(options.api_key.as_deref(), Some("key-from-file"));
        let tls = options.tls_options.expect("TLS options");
        assert_eq!(
            tls.server_root_ca_cert.as_deref(),
            Some(CERTIFICATE_PEM.as_bytes())
        );
        assert_eq!(tls.domain.as_deref(), Some("frontend.temporal.internal"));
        let identity = tls.client_tls_options.expect("client certificate");
        assert_eq!(identity.client_cert, CERTIFICATE_PEM.as_bytes());
        assert_eq!(identity.client_private_key, PRIVATE_KEY_PEM.as_bytes());
    }

    #[test]
    fn a_missing_file_names_its_setting() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let cert = write(&directory, "client.pem", CERTIFICATE_PEM);
        let key = write(&directory, "client.key", PRIVATE_KEY_PEM);
        let missing = directory.path().join("missing");
        let missing = missing.to_str().expect("unicode temporary path");

        let cases: [(&[(&'static str, &str)], &'static str); 4] = [
            (
                &[(TEMPORAL_TLS_SERVER_CA_FILE_ENV, missing)],
                TEMPORAL_TLS_SERVER_CA_FILE_ENV,
            ),
            (
                &[
                    (TEMPORAL_TLS_CLIENT_CERT_FILE_ENV, missing),
                    (TEMPORAL_TLS_CLIENT_KEY_FILE_ENV, &key),
                ],
                TEMPORAL_TLS_CLIENT_CERT_FILE_ENV,
            ),
            (
                &[
                    (TEMPORAL_TLS_CLIENT_CERT_FILE_ENV, &cert),
                    (TEMPORAL_TLS_CLIENT_KEY_FILE_ENV, missing),
                ],
                TEMPORAL_TLS_CLIENT_KEY_FILE_ENV,
            ),
            (
                &[(TEMPORAL_API_KEY_FILE_ENV, missing)],
                TEMPORAL_API_KEY_FILE_ENV,
            ),
        ];
        for (values, variable) in cases {
            let config = resolve(values).expect("settings are consistent");
            assert!(
                matches!(
                    config.connection_options(),
                    Err(TemporalConfigError::UnreadableFile { variable: named, source, .. })
                        if named == variable && source.kind() == io::ErrorKind::NotFound
                ),
                "{variable}"
            );
        }
    }

    #[test]
    fn swapped_or_unusable_files_are_refused_before_the_handshake() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let cert = write(&directory, "client.pem", CERTIFICATE_PEM);
        let key = write(&directory, "client.key", PRIVATE_KEY_PEM);
        let encrypted = write(
            &directory,
            "encrypted.key",
            "-----BEGIN ENCRYPTED PRIVATE KEY-----\nMIIE\n-----END ENCRYPTED PRIVATE KEY-----\n",
        );
        let empty = write(&directory, "empty", " \n");

        let cases: [(&[(&'static str, &str)], &'static str); 4] = [
            (
                &[(TEMPORAL_TLS_SERVER_CA_FILE_ENV, &key)],
                TEMPORAL_TLS_SERVER_CA_FILE_ENV,
            ),
            (
                &[
                    (TEMPORAL_TLS_CLIENT_CERT_FILE_ENV, &key),
                    (TEMPORAL_TLS_CLIENT_KEY_FILE_ENV, &cert),
                ],
                TEMPORAL_TLS_CLIENT_CERT_FILE_ENV,
            ),
            (
                &[
                    (TEMPORAL_TLS_CLIENT_CERT_FILE_ENV, &cert),
                    (TEMPORAL_TLS_CLIENT_KEY_FILE_ENV, &cert),
                ],
                TEMPORAL_TLS_CLIENT_KEY_FILE_ENV,
            ),
            (
                &[
                    (TEMPORAL_TLS_CLIENT_CERT_FILE_ENV, &cert),
                    (TEMPORAL_TLS_CLIENT_KEY_FILE_ENV, &encrypted),
                ],
                TEMPORAL_TLS_CLIENT_KEY_FILE_ENV,
            ),
        ];
        for (values, variable) in cases {
            let config = resolve(values).expect("settings are consistent");
            assert!(
                matches!(
                    config.connection_options(),
                    Err(TemporalConfigError::UnexpectedFileContents { variable: named, .. })
                        if named == variable
                ),
                "{variable}"
            );
        }

        let config = resolve(&[(TEMPORAL_API_KEY_FILE_ENV, &empty)]).expect("API key file");
        assert!(matches!(
            config.connection_options(),
            Err(TemporalConfigError::InvalidApiKey {
                variable: TEMPORAL_API_KEY_FILE_ENV
            })
        ));
    }

    #[test]
    fn the_api_key_never_appears_in_debug_output() {
        let config = resolve(&[(TEMPORAL_API_KEY_ENV, "key-that-must-stay-secret")])
            .expect("API key connection");
        let printed = format!("{config:?}");
        assert!(!printed.contains("key-that-must-stay-secret"));
        assert!(printed.contains("redacted"));
    }
}
