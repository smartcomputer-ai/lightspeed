//! Configuration failures must stop the binary before it attempts any service I/O.
use std::process::Command;

fn refuses(environment: &[(&str, &str)], expected: &str) {
    // Keep dotenv discovery away from the developer's checkout and use no
    // inherited credentials or service configuration.
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_lightspeed-runtime"))
        .env_clear()
        .current_dir(directory.path())
        .envs(environment.iter().copied())
        .output()
        .expect("run runtime configuration check");
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        error.contains(expected),
        "unexpected startup failure: {error}"
    );
}

#[test]
fn authentication_mode_is_required() {
    refuses(&[], "LIGHTSPEED_AUTH_MODE is required");
}

#[test]
fn authenticated_mode_requires_configured_tokens() {
    refuses(
        &[("LIGHTSPEED_AUTH_MODE", "authenticated")],
        "authenticated mode requires LIGHTSPEED_API_TOKEN",
    );
}

#[test]
fn malformed_tokens_refuse_startup() {
    refuses(
        &[
            ("LIGHTSPEED_AUTH_MODE", "authenticated"),
            ("LIGHTSPEED_API_TOKEN", "short"),
        ],
        "at least 32 characters",
    );
}

#[test]
fn overlapping_listeners_refuse_before_connecting() {
    refuses(
        &[
            ("LIGHTSPEED_AUTH_MODE", "authenticated"),
            (
                "LIGHTSPEED_API_TOKEN",
                "test-token-with-at-least-32-characters",
            ),
            ("LIGHTSPEED_PUBLIC_BIND", "127.0.0.1:18080"),
        ],
        "gateway listeners must use distinct bind addresses",
    );
}

#[test]
fn non_loopback_environment_listener_requires_its_own_origin() {
    refuses(
        &[
            ("LIGHTSPEED_AUTH_MODE", "authenticated"),
            (
                "LIGHTSPEED_API_TOKEN",
                "test-token-with-at-least-32-characters",
            ),
            ("LIGHTSPEED_ENVIRONMENT_GATEWAY_BIND", "0.0.0.0:18083"),
            (
                "LIGHTSPEED_PUBLIC_BASE_URL",
                "https://callbacks.example.test",
            ),
        ],
        "LIGHTSPEED_ENVIRONMENT_PUBLIC_BASE_URL or LIGHTSPEED_ENVIRONMENT_REGISTRATION_URLS is required",
    );
}
