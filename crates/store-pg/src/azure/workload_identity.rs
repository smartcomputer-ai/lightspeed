//! Microsoft Entra workload identity federation for Azure Blob storage.
//!
//! `object_store` performs the exchange, caches access tokens, and re-reads the
//! projected token file for each exchange. This module selects that provider
//! explicitly and closes two gaps that matter for credential privacy:
//!
//! - The provider's derived `Debug` prints its cached access token, so it is held
//!   behind [`FederatedCredentialProvider`], whose `Debug` shows only the file path.
//! - Request errors embed raw response bodies, so [`ScrubbingConnector`] reduces
//!   every error body to allowlisted diagnostics before the retry layer reads it.
use std::{error::Error as StdError, fmt, io, path::Path, sync::Arc};

use async_trait::async_trait;
use object_store::{
    CredentialProvider, HeaderMap,
    azure::{AzureCredential, AzureCredentialProvider, MicrosoftAzureBuilder},
    client::{
        HttpClient, HttpConnector, HttpError, HttpRequest, HttpResponse, HttpService,
        ReqwestConnector,
    },
};
use thiserror::Error;

const STORE: &str = "MicrosoftAzure";
/// Longer values are not diagnostic codes or identifiers, so they are withheld.
const MAX_DIAGNOSTIC_LEN: usize = 128;

/// Settings for exchanging a projected token for Azure Storage access tokens.
/// None of these values is secret; the token itself stays in the file.
#[derive(Clone, Debug)]
pub struct AzureWorkloadIdentity {
    pub client_id: String,
    pub tenant_id: String,
    /// Read again for each exchange and never canonicalized, so a projected
    /// volume can repoint its symlinks when the kubelet rotates the token.
    pub federated_token_file: String,
    /// HTTPS authority. `None` selects the public Azure cloud.
    pub authority_host: Option<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AzureWorkloadIdentityError {
    #[error("AZURE_CLIENT_ID must not be empty")]
    EmptyClientId,
    #[error("AZURE_TENANT_ID must be a tenant ID or domain name")]
    InvalidTenantId,
    #[error("AZURE_FEDERATED_TOKEN_FILE must be an absolute path")]
    InvalidTokenFile,
    #[error(
        "AZURE_AUTHORITY_HOST must be an HTTPS authority with a host and no credentials, path, query, or fragment"
    )]
    InvalidAuthorityHost,
}

impl AzureWorkloadIdentity {
    /// Check the settings without touching the file or the network, so commands
    /// that never reach Blob storage do not depend on either.
    pub fn validate(&self) -> Result<(), AzureWorkloadIdentityError> {
        if self.client_id.trim().is_empty() {
            return Err(AzureWorkloadIdentityError::EmptyClientId);
        }
        // The tenant becomes a path segment of the token URL.
        let tenant = self.tenant_id.as_str();
        if !tenant.starts_with(|c: char| c.is_ascii_alphanumeric())
            || !tenant
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        {
            return Err(AzureWorkloadIdentityError::InvalidTenantId);
        }
        if !Path::new(&self.federated_token_file).is_absolute() {
            return Err(AzureWorkloadIdentityError::InvalidTokenFile);
        }
        self.normalized_authority_host().map(|_| ())
    }

    /// The library joins the authority and tenant with a slash, so a configured
    /// trailing slash must not reach it.
    pub(super) fn normalized_authority_host(
        &self,
    ) -> Result<Option<String>, AzureWorkloadIdentityError> {
        let Some(authority) = &self.authority_host else {
            return Ok(None);
        };
        let url = url::Url::parse(authority)
            .map_err(|_| AzureWorkloadIdentityError::InvalidAuthorityHost)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(AzureWorkloadIdentityError::InvalidAuthorityHost);
        }
        Ok(Some(url.origin().ascii_serialization()))
    }
}

/// Install the federated credential on `builder`. Building creates the provider
/// without reading the token file or opening a connection.
pub(super) fn configure(
    builder: MicrosoftAzureBuilder,
    identity: AzureWorkloadIdentity,
) -> Result<MicrosoftAzureBuilder, object_store::Error> {
    identity.validate().map_err(generic)?;
    let authority_host = identity.normalized_authority_host().map_err(generic)?;
    let token_file = identity.federated_token_file;
    let builder = builder.with_http_connector(ScrubbingConnector);

    // The library's provider is reachable only through a built store. Its token
    // client shares this builder's connector, certificate trust, and retry policy.
    let mut federated = builder
        .clone()
        .with_credential_type("workload_identity")
        .with_client_id(identity.client_id)
        .with_tenant_id(identity.tenant_id)
        .with_federated_token_file(token_file.clone());
    if let Some(authority_host) = authority_host {
        federated = federated.with_authority_host(authority_host);
    }
    let inner = federated.build()?.credentials().clone();
    Ok(builder.with_credentials(Arc::new(FederatedCredentialProvider { inner, token_file })))
}

fn generic(error: impl StdError + Send + Sync + 'static) -> object_store::Error {
    object_store::Error::Generic {
        store: STORE,
        source: Box::new(error),
    }
}

/// The stage at which an access token could not be obtained. Holds rendered text
/// rather than the library's error so that nothing else is reachable as a source.
#[derive(Debug, Error)]
pub enum TokenAcquisitionError {
    #[error(
        "could not read the projected token file '{path}' named by AZURE_FEDERATED_TOKEN_FILE{}",
        reason.map(|kind| format!(": {kind}")).unwrap_or_default()
    )]
    TokenFile {
        path: String,
        reason: Option<io::ErrorKind>,
    },
    #[error("Microsoft Entra token exchange failed: {detail}")]
    Exchange { detail: String },
    #[error("Microsoft Entra returned a token response that could not be read")]
    InvalidResponse,
}

struct FederatedCredentialProvider {
    inner: AzureCredentialProvider,
    token_file: String,
}

impl fmt::Debug for FederatedCredentialProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FederatedCredentialProvider")
            .field("token_file", &self.token_file)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl CredentialProvider for FederatedCredentialProvider {
    type Credential = AzureCredential;

    async fn get_credential(&self) -> object_store::Result<Arc<AzureCredential>> {
        self.inner
            .get_credential()
            .await
            .map_err(|error| generic(classify_token_error(&error, &self.token_file)))
    }
}

/// The library's credential error type is private, but its variants differ in
/// their source: none for the token file, `HttpError` for an unreadable token
/// response, and an opaque retry error for the exchange request.
fn classify_token_error(error: &object_store::Error, token_file: &str) -> TokenAcquisitionError {
    let object_store::Error::Generic { source, .. } = error else {
        return TokenAcquisitionError::Exchange {
            detail: "details withheld".into(),
        };
    };
    match source.source() {
        None => TokenAcquisitionError::TokenFile {
            path: token_file.to_owned(),
            reason: std::fs::metadata(token_file).err().map(|e| e.kind()),
        },
        Some(inner) if inner.is::<HttpError>() => TokenAcquisitionError::InvalidResponse,
        Some(_) => TokenAcquisitionError::Exchange {
            detail: render_chain(source.as_ref()),
        },
    }
}

/// Keep transport causes such as certificate rejections. Response bodies in this
/// text have already passed through [`ScrubbingConnector`].
pub(super) fn render_chain(error: &(dyn StdError + 'static)) -> String {
    let mut rendered = error.to_string();
    let mut current = error.source();
    while let Some(source) = current {
        let text = source.to_string();
        if !rendered.contains(&text) {
            rendered.push_str(": ");
            rendered.push_str(&text);
        }
        current = source.source();
    }
    rendered
}

#[derive(Debug)]
struct ScrubbingConnector;

impl HttpConnector for ScrubbingConnector {
    fn connect(&self, options: &object_store::ClientOptions) -> object_store::Result<HttpClient> {
        Ok(HttpClient::new(ScrubbingService(
            ReqwestConnector::default().connect(options)?,
        )))
    }
}

#[derive(Debug)]
struct ScrubbingService(HttpClient);

#[async_trait]
impl HttpService for ScrubbingService {
    async fn call(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let response = self.0.execute(request).await?;
        if !(response.status().is_client_error() || response.status().is_server_error()) {
            return Ok(response);
        }
        let (parts, body) = response.into_parts();
        let body = body.bytes().await?;
        let summary = diagnostic_summary(&parts.headers, &body);
        Ok(HttpResponse::from_parts(parts, summary.into()))
    }
}

/// Reduce an Entra or Azure Storage error to fields that identify the failure
/// and locate it in the service's logs. Free text is dropped because it can
/// repeat request content; Entra's `AADSTS` code identifies the same failure.
pub(super) fn diagnostic_summary(headers: &HeaderMap, body: &[u8]) -> String {
    let mut fields = Vec::new();
    let mut push = |name: &str, value: Option<&str>| {
        if let Some(value) = value.filter(is_diagnostic_value) {
            fields.push(format!("{name}={value}"));
        }
    };
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    let text = String::from_utf8_lossy(body);

    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(body) {
        push("error", json["error"].as_str());
        for code in json["error_codes"].as_array().into_iter().flatten() {
            if let Some(code) = code.as_u64() {
                push("code", Some(&format!("AADSTS{code}")));
            }
        }
        push("trace_id", json["trace_id"].as_str());
        push("correlation_id", json["correlation_id"].as_str());
    }
    push(
        "storage_error",
        header("x-ms-error-code").or_else(|| xml_element(&text, "Code")),
    );
    push("request_id", header("x-ms-request-id"));

    if fields.is_empty() {
        "details withheld".into()
    } else {
        fields.join(" ")
    }
}

fn is_diagnostic_value(value: &&str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_DIAGNOSTIC_LEN
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn xml_element<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let (_, rest) = text.split_once(&format!("<{name}>"))?;
    let (value, _) = rest.split_once(&format!("</{name}>"))?;
    Some(value)
}

#[cfg(test)]
mod tests;
