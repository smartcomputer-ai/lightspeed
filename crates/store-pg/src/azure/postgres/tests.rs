use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use uuid::Uuid;

use super::{
    super::test_support::{Handler, Response, Server, TestCa, entra},
    *,
};

const ASSERTION: &str = "secret-assertion.jwt.value";
const ROTATED_ASSERTION: &str = "secret-rotated-assertion.jwt.value";

fn source(ca: &TestCa, login: &Server, token_file: &Path) -> TokenSource {
    TokenSource::new(
        ca.reqwest_client(),
        AzureWorkloadIdentity {
            client_id: "client-id".into(),
            tenant_id: "tenant-id".into(),
            federated_token_file: token_file.to_str().unwrap().into(),
            authority_host: Some(format!("{}/", login.url())),
        },
    )
    .unwrap()
}

fn write_token(directory: &Path, contents: &str) -> std::path::PathBuf {
    let path = directory.join("token");
    std::fs::write(&path, contents).unwrap();
    path
}

fn failing(status: u16, body: &str) -> Arc<Handler> {
    let body = body.to_owned();
    Arc::new(move |_| Response::new(status, body.clone()))
}

#[test]
fn urls_must_name_the_identity_verify_tls_and_hold_no_password() {
    let base = "postgres://identity@db.example/lightspeed";
    assert!(connect_options(&format!("{base}?sslmode=verify-full")).is_ok());
    for (url, expected) in [
        ("not a url".to_owned(), "InvalidUrl"),
        (
            "postgres://db.example/lightspeed?sslmode=verify-full".to_owned(),
            "MissingUser",
        ),
        (
            "postgres://identity:secret-password@db.example/lightspeed?sslmode=verify-full"
                .to_owned(),
            "PasswordInUrl",
        ),
        (
            format!("{base}?sslmode=verify-full&password=secret-password"),
            "PasswordInUrl",
        ),
        (base.to_owned(), "TlsNotVerified"),
        (format!("{base}?sslmode=require"), "TlsNotVerified"),
        (format!("{base}?sslmode=verify-ca"), "TlsNotVerified"),
    ] {
        let error = connect_options(&url).unwrap_err();
        assert!(
            format!("{error:?}").starts_with(expected),
            "{url}: {error:?}"
        );
        assert!(!error.to_string().contains("secret-"), "{error}");
    }
}

#[test]
fn deployment_pools_limit_reconnects_and_still_check_connections_before_use() {
    let options = crate::postgres_pool_options();
    assert_eq!(options.get_min_connections(), 2);
    assert_eq!(options.get_max_lifetime(), Some(Duration::from_secs(3600)));
    assert_eq!(options.get_idle_timeout(), Some(Duration::from_secs(600)));
    assert!(options.get_test_before_acquire());
}

#[test]
fn renewal_waits_half_a_lifetime_but_never_less_than_the_minimum() {
    let minimum = Duration::from_secs(30);
    assert_eq!(
        refresh_delay(Duration::from_secs(3600), minimum),
        Duration::from_secs(1800)
    );
    assert_eq!(refresh_delay(Duration::from_secs(10), minimum), minimum);
    assert_eq!(refresh_delay(Duration::ZERO, minimum), minimum);
}

#[tokio::test(flavor = "current_thread")]
async fn the_exchange_presents_the_projected_token_for_the_postgres_scope() {
    let ca = TestCa::new();
    let directory = tempfile::tempdir().unwrap();
    let token_file = write_token(directory.path(), ASSERTION);
    let login = Server::start(ca.acceptor("localhost"), entra(3600)).await;

    let token = source(&ca, &login, &token_file).fetch().await.unwrap();

    assert_eq!(token.value, "secret-access-token-1");
    assert_eq!(token.expires_in, Duration::from_secs(3600));
    let requests = login.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/tenant-id/oauth2/v2.0/token");
    let form = requests[0].form();
    assert_eq!(form["grant_type"], "client_credentials");
    assert_eq!(form["client_id"], "client-id");
    assert_eq!(form["client_assertion"], ASSERTION);
    assert_eq!(
        form["client_assertion_type"],
        "urn:ietf:params:oauth:client-assertion-type:jwt-bearer"
    );
    assert_eq!(
        form["scope"],
        "https://ossrdbms-aad.database.windows.net/.default"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn each_exchange_reads_the_current_projected_token() {
    let ca = TestCa::new();
    let directory = tempfile::tempdir().unwrap();
    let token_file = write_token(directory.path(), ASSERTION);
    let login = Server::start(ca.acceptor("localhost"), entra(3600)).await;
    let source = source(&ca, &login, &token_file);

    source.fetch().await.unwrap();
    let staged = directory.path().join("staged");
    std::fs::write(&staged, ROTATED_ASSERTION).unwrap();
    std::fs::rename(&staged, &token_file).unwrap();
    source.fetch().await.unwrap();

    let requests = login.requests();
    assert_eq!(requests[0].form()["client_assertion"], ASSERTION);
    assert_eq!(requests[1].form()["client_assertion"], ROTATED_ASSERTION);
}

#[tokio::test(flavor = "current_thread")]
async fn a_rejected_exchange_reports_diagnostics_but_not_the_response_text() {
    let ca = TestCa::new();
    let directory = tempfile::tempdir().unwrap();
    let token_file = write_token(directory.path(), ASSERTION);
    let body = format!(
        r#"{{"error":"invalid_client","error_description":"bad {ASSERTION}","error_codes":[70021],
        "trace_id":"trace-1","access_token":"secret-echoed"}}"#
    );
    let login = Server::start(ca.acceptor("localhost"), failing(401, &body)).await;

    let Err(error) = source(&ca, &login, &token_file).fetch().await else {
        panic!("expected a rejected exchange")
    };

    let TokenAcquisitionError::Exchange { detail } = &error else {
        panic!("expected the exchange stage: {error:?}")
    };
    for expected in [
        "401",
        "error=invalid_client",
        "AADSTS70021",
        "trace_id=trace-1",
    ] {
        assert!(detail.contains(expected), "missing {expected}: {detail}");
    }
    let rendered = format!("{error} {error:?}");
    assert!(!rendered.contains("secret-"), "{rendered}");
}

#[tokio::test(flavor = "current_thread")]
async fn an_unusable_token_response_does_not_repeat_its_body() {
    let ca = TestCa::new();
    let directory = tempfile::tempdir().unwrap();
    let token_file = write_token(directory.path(), ASSERTION);
    let body = r#"{"access_token":"secret-token","expires_in":"secret-expiry"}"#;
    let login = Server::start(ca.acceptor("localhost"), failing(200, body)).await;

    let Err(error) = source(&ca, &login, &token_file).fetch().await else {
        panic!("expected an unusable response")
    };

    assert!(matches!(error, TokenAcquisitionError::InvalidResponse));
    assert!(!format!("{error} {error:?}").contains("secret-"));
}

#[tokio::test(flavor = "current_thread")]
async fn an_unreadable_token_file_is_reported_without_contacting_entra() {
    let ca = TestCa::new();
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing");
    let login = Server::start(ca.acceptor("localhost"), entra(3600)).await;

    let Err(error) = source(&ca, &login, &missing).fetch().await else {
        panic!("expected an unreadable file")
    };

    let TokenAcquisitionError::TokenFile { path, reason } = &error else {
        panic!("expected the token file stage: {error:?}")
    };
    assert_eq!(path, missing.to_str().unwrap());
    assert_eq!(*reason, Some(std::io::ErrorKind::NotFound));
    assert!(login.requests().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn untrusted_or_misnamed_token_endpoints_receive_no_assertion() {
    let trusted = TestCa::new();
    let untrusted = TestCa::new();
    let directory = tempfile::tempdir().unwrap();
    let token_file = write_token(directory.path(), ASSERTION);

    for acceptor in [
        untrusted.acceptor("localhost"),
        trusted.acceptor("other.example"),
    ] {
        let login = Server::start(acceptor, entra(3600)).await;
        let Err(error) = source(&trusted, &login, &token_file).fetch().await else {
            panic!("expected a certificate rejection")
        };
        assert!(error.to_string().contains("certificate"), "{error}");
        assert!(login.requests().is_empty());
    }
}

#[test]
fn plaintext_authorities_are_rejected_before_any_request() {
    let identity = AzureWorkloadIdentity {
        client_id: "client-id".into(),
        tenant_id: "tenant-id".into(),
        federated_token_file: "/token".into(),
        authority_host: Some("http://login.example".into()),
    };
    assert!(matches!(
        TokenSource::new(reqwest::Client::new(), identity),
        Err(AzureWorkloadIdentityError::InvalidAuthorityHost)
    ));
}

/// Fake Entra issues the role's real password as the token, so the database
/// authenticates each new connection against whatever was exchanged last.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires LIGHTSPEED_TEST_POSTGRES_URL with password authentication and rights to create roles"]
async fn new_connections_use_the_renewed_token_and_open_ones_keep_working() {
    let admin_url =
        std::env::var("LIGHTSPEED_TEST_POSTGRES_URL").expect("explicit test PostgreSQL URL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await
        .unwrap();
    let role = format!("entra_rotation_{}", Uuid::new_v4().simple());
    let alter_password = |password: &str| format!("ALTER ROLE {role} PASSWORD '{password}'");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE ROLE {role} LOGIN PASSWORD 'first-password'"
    )))
    .execute(&admin)
    .await
    .unwrap();

    let current = Arc::new(Mutex::new("first-password".to_owned()));
    let issued = current.clone();
    let handler: Arc<Handler> = Arc::new(move |_| {
        Response::new(
            200,
            format!(
                r#"{{"access_token":"{}","expires_in":1}}"#,
                issued.lock().unwrap()
            ),
        )
    });
    let ca = TestCa::new();
    let directory = tempfile::tempdir().unwrap();
    let token_file = write_token(directory.path(), ASSERTION);
    let login = Server::start(ca.acceptor("localhost"), handler).await;
    let options = PgConnectOptions::from_str(&admin_url)
        .unwrap()
        .username(&role);

    let outcome = async {
        let pool = connect(
            options.clone(),
            PgPoolOptions::new().max_connections(2),
            source(&ca, &login, &token_file),
            Duration::from_millis(50),
        )
        .await
        .unwrap();
        let mut held = pool.acquire().await.unwrap();

        sqlx::raw_sql(sqlx::AssertSqlSafe(alter_password("second-password")))
            .execute(&admin)
            .await
            .unwrap();
        *current.lock().unwrap() = "second-password".to_owned();
        let exchanges = login.requests().len();
        while login.requests().len() < exchanges + 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // The old password really is dead, so the pool can only be using the new one.
        assert!(
            <sqlx::PgConnection as sqlx::Connection>::connect_with(
                &options.clone().password("first-password")
            )
            .await
            .is_err()
        );
        let mut opened = pool.acquire().await.unwrap();
        let user: String = sqlx::query_scalar("SELECT current_user::text")
            .fetch_one(&mut *opened)
            .await
            .unwrap();
        assert_eq!(user, role);
        let survived: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(&mut *held)
            .await
            .unwrap();
        assert_eq!(survived, 1);

        drop((held, opened));
        pool.close().await;
    };
    let outcome =
        futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(outcome)).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP ROLE {role}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
