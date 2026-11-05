//! Runs the library's real exchange and Blob client over TLS against a local
//! fake of Entra and Azure Storage, so certificate checks and wire content are
//! the ones production uses. Certificates are generated per test.
use std::{
    collections::HashMap,
    error::Error as StdError,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use object_store::{ClientOptions, ObjectStore, ObjectStoreExt as _, RetryConfig};
use tokio::net::TcpListener;

use super::{
    super::{
        test_support::{Handler, Response, Server, TestCa, entra},
        *,
    },
    *,
};

const CLIENT_ID: &str = "client-id";
const TENANT_ID: &str = "tenant-id";
const ASSERTION: &str = "secret-assertion.jwt.value";
const ROTATED_ASSERTION: &str = "secret-rotated-assertion.jwt.value";
const LAST_MODIFIED: &str = "Wed, 21 Oct 2015 07:28:00 GMT";

fn storage() -> Arc<Handler> {
    let blobs = Mutex::new(HashMap::<String, Vec<u8>>::new());
    Arc::new(move |request| {
        let mut blobs = blobs.lock().unwrap();
        let mut response = match request.method.as_str() {
            "PUT" => {
                blobs.insert(request.path.clone(), request.body.clone());
                Response::new(201, Vec::new())
            }
            _ => match blobs.get(&request.path) {
                Some(body) => Response::new(200, body.clone()),
                None => Response::new(404, "<Error><Code>BlobNotFound</Code></Error>"),
            },
        };
        response.headers.push(("ETag", "\"etag\"".into()));
        response
            .headers
            .push(("Last-Modified", LAST_MODIFIED.into()));
        response
    })
}

fn failing(status: u16, headers: Vec<(&'static str, String)>, body: &str) -> Arc<Handler> {
    let body = body.to_owned();
    Arc::new(move |_| Response {
        status,
        headers: headers.clone(),
        body: body.clone().into_bytes(),
    })
}

fn write_token(directory: &Path, contents: &str) -> PathBuf {
    let path = directory.join("azure-identity-token");
    std::fs::write(&path, contents).unwrap();
    path
}

fn no_retries() -> RetryConfig {
    RetryConfig {
        max_retries: 0,
        retry_timeout: Duration::from_secs(5),
        ..RetryConfig::default()
    }
}

fn store(
    authority: &str,
    blob_endpoint: &str,
    token_file: &Path,
    options: ClientOptions,
) -> Arc<dyn ObjectStore> {
    build(
        AzureObjectStoreConfig {
            account: "account".into(),
            container: "container".into(),
            endpoint: Some(format!("{blob_endpoint}/account")),
            credential: AzureObjectStoreCredential::WorkloadIdentity(AzureWorkloadIdentity {
                client_id: CLIENT_ID.into(),
                tenant_id: TENANT_ID.into(),
                federated_token_file: token_file.to_str().unwrap().into(),
                authority_host: Some(authority.into()),
            }),
        },
        options,
        no_retries(),
    )
    .unwrap()
}

fn location() -> object_store::path::Path {
    object_store::path::Path::from("prefix/blob")
}

fn full_chain(error: &object_store::Error) -> String {
    let mut text = format!("{error} | {error:?}");
    let mut current = error.source();
    while let Some(source) = current {
        text.push_str(&format!(" | {source} | {source:?}"));
        current = source.source();
    }
    text
}

fn assert_private(text: &str) {
    assert!(!text.contains("secret-"), "credential exposed: {text}");
}

fn token_error(error: &object_store::Error) -> &TokenAcquisitionError {
    let object_store::Error::Generic { source, .. } = error else {
        panic!("expected a generic error: {error:?}")
    };
    source
        .downcast_ref()
        .unwrap_or_else(|| panic!("expected a token acquisition error: {error:?}"))
}

#[tokio::test(flavor = "current_thread")]
async fn exchange_presents_the_projected_token_and_blobs_use_the_bearer_token() {
    let ca = TestCa::new();
    let directory = tempfile::tempdir().unwrap();
    let token_file = write_token(directory.path(), ASSERTION);
    let login = Server::start(ca.acceptor("localhost"), entra(3600)).await;
    let blobs = Server::start(ca.acceptor("localhost"), storage()).await;
    // The trailing slash must not produce a double slash in the token path.
    let authority = format!("{}/", login.url());
    let store = store(&authority, &blobs.url(), &token_file, ca.client_options());

    store.put(&location(), vec![1, 2, 3].into()).await.unwrap();
    let read = store.get(&location()).await.unwrap().bytes().await.unwrap();
    assert_eq!(read.as_ref(), [1, 2, 3]);

    let exchanges = login.requests();
    assert_eq!(exchanges.len(), 1, "the provider caches the access token");
    let exchange = &exchanges[0];
    assert_eq!(exchange.method, "POST");
    assert_eq!(exchange.path, format!("/{TENANT_ID}/oauth2/v2.0/token"));
    let form = exchange.form();
    assert_eq!(form["grant_type"], "client_credentials");
    assert_eq!(form["client_id"], CLIENT_ID);
    assert_eq!(form["client_assertion"], ASSERTION);
    assert_eq!(
        form["client_assertion_type"],
        "urn:ietf:params:oauth:client-assertion-type:jwt-bearer"
    );
    assert_eq!(form["scope"], "https://storage.azure.com/.default");

    let requests = blobs.requests();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert_eq!(
            request.headers["authorization"],
            "Bearer secret-access-token-1"
        );
        assert!(request.path.starts_with("/account/container/prefix/blob"));
        assert!(!request.path.contains("sig="));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn each_exchange_reads_the_current_token_through_a_rotated_symlink() {
    let ca = TestCa::new();
    let directory = tempfile::tempdir().unwrap();
    // Mirror a projected volume: the path is a symlink into a versioned directory.
    let version = |name: &str, contents: &str| {
        let path = directory.path().join(name);
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("token"), contents).unwrap();
        path
    };
    let first = version("version-1", ASSERTION);
    let second = version("version-2", ROTATED_ASSERTION);
    let data = directory.path().join("data");
    std::os::unix::fs::symlink(&first, &data).unwrap();
    let token_file = directory.path().join("token");
    std::os::unix::fs::symlink(data.join("token"), &token_file).unwrap();

    // Immediately expired tokens force an exchange for every request.
    let login = Server::start(ca.acceptor("localhost"), entra(0)).await;
    let blobs = Server::start(ca.acceptor("localhost"), storage()).await;
    let store = store(&login.url(), &blobs.url(), &token_file, ca.client_options());

    store.put(&location(), vec![1].into()).await.unwrap();

    let staged = directory.path().join("data-next");
    std::os::unix::fs::symlink(&second, &staged).unwrap();
    std::fs::rename(&staged, &data).unwrap();
    std::fs::remove_dir_all(&first).unwrap();

    store.put(&location(), vec![2].into()).await.unwrap();

    let assertions: Vec<_> = login
        .requests()
        .iter()
        .map(|request| request.form()["client_assertion"].clone())
        .collect();
    assert_eq!(assertions, [ASSERTION, ROTATED_ASSERTION]);
    let bearers: Vec<_> = blobs
        .requests()
        .iter()
        .map(|request| request.headers["authorization"].clone())
        .collect();
    assert_eq!(
        bearers,
        [
            "Bearer secret-access-token-1",
            "Bearer secret-access-token-2"
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_replaced_token_file_is_read_by_the_next_exchange() {
    let ca = TestCa::new();
    let directory = tempfile::tempdir().unwrap();
    let token_file = write_token(directory.path(), ASSERTION);
    let login = Server::start(ca.acceptor("localhost"), entra(0)).await;
    let blobs = Server::start(ca.acceptor("localhost"), storage()).await;
    let store = store(&login.url(), &blobs.url(), &token_file, ca.client_options());

    store.put(&location(), vec![1].into()).await.unwrap();
    let staged = directory.path().join("staged");
    std::fs::write(&staged, ROTATED_ASSERTION).unwrap();
    std::fs::rename(&staged, &token_file).unwrap();
    store.put(&location(), vec![2].into()).await.unwrap();

    let exchanges = login.requests();
    assert_eq!(exchanges[0].form()["client_assertion"], ASSERTION);
    assert_eq!(exchanges[1].form()["client_assertion"], ROTATED_ASSERTION);
}

#[tokio::test(flavor = "current_thread")]
async fn untrusted_or_misnamed_token_endpoints_receive_no_assertion() {
    let trusted = TestCa::new();
    let untrusted = TestCa::new();
    let directory = tempfile::tempdir().unwrap();
    let token_file = write_token(directory.path(), ASSERTION);
    let blobs = Server::start(trusted.acceptor("localhost"), storage()).await;

    for acceptor in [
        untrusted.acceptor("localhost"),
        trusted.acceptor("other.example"),
    ] {
        let login = Server::start(acceptor, entra(3600)).await;
        let store = store(
            &login.url(),
            &blobs.url(),
            &token_file,
            trusted.client_options(),
        );
        let error = store.put(&location(), vec![1].into()).await.unwrap_err();
        assert!(matches!(
            token_error(&error),
            TokenAcquisitionError::Exchange { .. }
        ));
        assert!(error.to_string().contains("certificate"), "{error}");
        assert_private(&full_chain(&error));
        assert!(login.requests().is_empty());
    }
    assert!(blobs.requests().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn untrusted_or_misnamed_blob_endpoints_receive_no_bearer_token() {
    let trusted = TestCa::new();
    let untrusted = TestCa::new();
    let directory = tempfile::tempdir().unwrap();
    let token_file = write_token(directory.path(), ASSERTION);
    let login = Server::start(trusted.acceptor("localhost"), entra(3600)).await;

    for acceptor in [
        untrusted.acceptor("localhost"),
        trusted.acceptor("other.example"),
    ] {
        let blobs = Server::start(acceptor, storage()).await;
        let store = store(
            &login.url(),
            &blobs.url(),
            &token_file,
            trusted.client_options(),
        );
        let error = store.put(&location(), vec![1].into()).await.unwrap_err();
        assert!(full_chain(&error).contains("certificate"), "{error:?}");
        assert_private(&full_chain(&error));
        assert!(blobs.requests().is_empty());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_rejected_exchange_reports_diagnostics_but_not_the_response_text() {
    let ca = TestCa::new();
    let directory = tempfile::tempdir().unwrap();
    let token_file = write_token(directory.path(), ASSERTION);
    let body = format!(
        r#"{{"error":"invalid_client","error_description":"bad assertion {ASSERTION}",
        "error_codes":[70021],"trace_id":"11111111-2222","correlation_id":"33333333-4444",
        "access_token":"secret-echoed-token"}}"#
    );
    let login = Server::start(ca.acceptor("localhost"), failing(401, Vec::new(), &body)).await;
    let blobs = Server::start(ca.acceptor("localhost"), storage()).await;
    let store = store(&login.url(), &blobs.url(), &token_file, ca.client_options());

    let error = store.put(&location(), vec![1].into()).await.unwrap_err();

    let TokenAcquisitionError::Exchange { detail } = token_error(&error) else {
        panic!("expected the exchange stage: {error:?}")
    };
    for expected in [
        "401",
        "error=invalid_client",
        "AADSTS70021",
        "trace_id=11111111-2222",
        "correlation_id=33333333-4444",
    ] {
        assert!(detail.contains(expected), "missing {expected}: {detail}");
    }
    assert_private(&full_chain(&error));
    assert_eq!(login.requests().len(), 1);
    assert!(blobs.requests().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn an_unusable_token_response_does_not_repeat_its_body() {
    let ca = TestCa::new();
    let directory = tempfile::tempdir().unwrap();
    let token_file = write_token(directory.path(), ASSERTION);
    let body = r#"{"access_token":"secret-token","expires_in":"secret-expiry"}"#;
    let login = Server::start(ca.acceptor("localhost"), failing(200, Vec::new(), body)).await;
    let blobs = Server::start(ca.acceptor("localhost"), storage()).await;
    let store = store(&login.url(), &blobs.url(), &token_file, ca.client_options());

    let error = store.put(&location(), vec![1].into()).await.unwrap_err();

    assert!(matches!(
        token_error(&error),
        TokenAcquisitionError::InvalidResponse
    ));
    assert_private(&full_chain(&error));
}

#[tokio::test(flavor = "current_thread")]
async fn an_unreadable_token_file_is_reported_without_contacting_entra() {
    let ca = TestCa::new();
    let directory = tempfile::tempdir().unwrap();
    let token_file = directory.path().join("missing");
    let login = Server::start(ca.acceptor("localhost"), entra(3600)).await;
    let blobs = Server::start(ca.acceptor("localhost"), storage()).await;
    let store = store(&login.url(), &blobs.url(), &token_file, ca.client_options());

    let error = store.put(&location(), vec![1].into()).await.unwrap_err();

    let TokenAcquisitionError::TokenFile { path, reason } = token_error(&error) else {
        panic!("expected the token file stage: {error:?}")
    };
    assert_eq!(path, token_file.to_str().unwrap());
    assert_eq!(*reason, Some(io::ErrorKind::NotFound));
    assert!(error.to_string().contains("AZURE_FEDERATED_TOKEN_FILE"));
    assert!(login.requests().is_empty());
    assert!(blobs.requests().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn blob_authorization_failures_stay_typed_and_private() {
    let ca = TestCa::new();
    let directory = tempfile::tempdir().unwrap();
    let token_file = write_token(directory.path(), ASSERTION);
    let login = Server::start(ca.acceptor("localhost"), entra(3600)).await;
    let body = "<?xml version=\"1.0\"?><Error><Code>AuthorizationPermissionMismatch</Code>\
        <Message>echoed Bearer secret-access-token-1</Message></Error>";

    for (status, code) in [
        (403, "AuthorizationPermissionMismatch"),
        (401, "InvalidAuth"),
    ] {
        let headers = vec![
            ("x-ms-error-code", code.to_owned()),
            ("x-ms-request-id", "request-1234".to_owned()),
        ];
        let blobs = Server::start(ca.acceptor("localhost"), failing(status, headers, body)).await;
        let store = store(&login.url(), &blobs.url(), &token_file, ca.client_options());

        let error = store.put(&location(), vec![1].into()).await.unwrap_err();

        match status {
            403 => assert!(matches!(
                error,
                object_store::Error::PermissionDenied { .. }
            )),
            _ => assert!(matches!(error, object_store::Error::Unauthenticated { .. })),
        }
        let text = full_chain(&error);
        assert!(text.contains(&format!("storage_error={code}")), "{text}");
        assert!(text.contains("request_id=request-1234"), "{text}");
        assert_private(&text);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn store_debug_output_hides_the_cached_access_token() {
    let ca = TestCa::new();
    let directory = tempfile::tempdir().unwrap();
    let token_file = write_token(directory.path(), ASSERTION);
    let login = Server::start(ca.acceptor("localhost"), entra(3600)).await;
    let blobs = Server::start(ca.acceptor("localhost"), storage()).await;
    let store = store(&login.url(), &blobs.url(), &token_file, ca.client_options());
    store.put(&location(), vec![1].into()).await.unwrap();

    // The cache is populated here, so a leak would show in this output.
    assert_eq!(login.requests().len(), 1);
    let debug = format!("{store:?} {store}");
    assert!(debug.contains("FederatedCredentialProvider"), "{debug}");
    assert_private(&debug);
}

#[tokio::test(flavor = "current_thread")]
async fn building_the_store_opens_no_connection() {
    let ca = TestCa::new();
    let login = Server::start(ca.acceptor("localhost"), entra(3600)).await;
    let blobs = Server::start(ca.acceptor("localhost"), storage()).await;

    let _store = store(
        &login.url(),
        &blobs.url(),
        Path::new("/nonexistent/projected/token"),
        ca.client_options(),
    );
    tokio::task::yield_now().await;

    assert!(login.requests().is_empty());
    assert!(blobs.requests().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn plaintext_authorities_and_endpoints_are_never_used() {
    let ca = TestCa::new();
    let directory = tempfile::tempdir().unwrap();
    let token_file = write_token(directory.path(), ASSERTION);

    let plaintext_authority = build(
        AzureObjectStoreConfig {
            account: "account".into(),
            container: "container".into(),
            endpoint: None,
            credential: AzureObjectStoreCredential::WorkloadIdentity(AzureWorkloadIdentity {
                client_id: CLIENT_ID.into(),
                tenant_id: TENANT_ID.into(),
                federated_token_file: token_file.to_str().unwrap().into(),
                authority_host: Some("http://localhost:9".into()),
            }),
        },
        ca.client_options(),
        no_retries(),
    );
    assert!(matches!(
        plaintext_authority,
        Err(object_store::Error::Generic { .. })
    ));

    // Configuration rejects this endpoint first; the client must still refuse it
    // for a direct caller, after a real token has been obtained.
    let login = Server::start(ca.acceptor("localhost"), entra(3600)).await;
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let endpoint = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let store = store(&login.url(), &endpoint, &token_file, ca.client_options());

    let error = store.put(&location(), vec![1].into()).await.unwrap_err();

    assert_eq!(login.requests().len(), 1);
    assert!(full_chain(&error).contains("scheme"), "{error:?}");
    assert_private(&full_chain(&error));
    let accepted = tokio::time::timeout(Duration::from_millis(50), listener.accept()).await;
    assert!(accepted.is_err(), "the plaintext endpoint was contacted");
}

#[test]
fn authority_hosts_normalize_to_one_form() {
    let identity = |authority: Option<&str>| AzureWorkloadIdentity {
        client_id: CLIENT_ID.into(),
        tenant_id: TENANT_ID.into(),
        federated_token_file: "/token".into(),
        authority_host: authority.map(Into::into),
    };
    assert_eq!(identity(None).normalized_authority_host(), Ok(None));
    for authority in [
        "https://login.microsoftonline.com",
        "https://login.microsoftonline.com/",
        "HTTPS://LOGIN.microsoftonline.com/",
        "https://login.microsoftonline.com:443/",
    ] {
        assert_eq!(
            identity(Some(authority)).normalized_authority_host(),
            Ok(Some("https://login.microsoftonline.com".to_owned())),
            "{authority}"
        );
    }
    assert_eq!(
        identity(Some("https://login.example:8443/")).normalized_authority_host(),
        Ok(Some("https://login.example:8443".to_owned()))
    );
}

#[test]
fn diagnostic_summaries_keep_only_identifiers() {
    let empty = HeaderMap::new();
    assert_eq!(diagnostic_summary(&empty, b""), "details withheld");
    assert_eq!(
        diagnostic_summary(&empty, b"plain secret-text"),
        "details withheld"
    );
    // Dotted values such as JWTs are not identifiers, even in an allowlisted field.
    assert_eq!(
        diagnostic_summary(&empty, br#"{"error":"header.payload.signature"}"#),
        "details withheld"
    );
    let oversized = format!(r#"{{"error":"{}"}}"#, "a".repeat(MAX_DIAGNOSTIC_LEN + 1));
    assert_eq!(
        diagnostic_summary(&empty, oversized.as_bytes()),
        "details withheld"
    );
    assert_eq!(
        diagnostic_summary(&empty, b"<Error><Code>BlobNotFound</Code></Error>"),
        "storage_error=BlobNotFound"
    );
}
