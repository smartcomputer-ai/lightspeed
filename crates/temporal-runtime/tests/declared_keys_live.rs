//! Live acceptance for keys whose secret the caller supplies: an API key and
//! an environment registration key are declared through the public API,
//! declared again without effect, converge on changed settings, and answer
//! to a replacement secret at once.

use std::sync::Arc;

use api::{
    AccessScope, AgentApiErrorKind, AgentApiService, DeploymentApiKeyCreateParams,
    DeploymentApiKeyPutParams, DeploymentApiKeyRevokeParams, DeploymentApiKeyRotateParams,
    DeploymentApiService, DeploymentUniverseCreateParams, EnvironmentIdentityModeView,
    EnvironmentRegistrationKeyCreateParams, EnvironmentRegistrationKeyPutParams,
    EnvironmentRegistrationKeyReadParams, EnvironmentRegistrationKeyRevokeParams,
    EnvironmentRegistrationKeyStatusView, EnvironmentRegistrationSecretView, MethodGroup,
};
use axum::http::HeaderMap;
use environments::EnvironmentRegistrationKeyStore;
use store_pg::PgApiKeyStore;
use temporal_runtime::{
    DeploymentStores, UniverseRuntime,
    gateway::{
        GatewayDeploymentApi,
        authentication::authenticate,
        request_context::{RequestContext, with_request_context as calling},
    },
};
use temporal_workflow::{DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET, connect_temporal};
use uuid::Uuid;

fn deployment_caller() -> RequestContext {
    RequestContext::local(AccessScope::Deployment)
}

/// A secret in the accepted form, distinct per `seed`.
fn secret(prefix: &str, seed: u128) -> String {
    use base64::Engine as _;
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(&seed.to_be_bytes());
    bytes[16..].copy_from_slice(&Uuid::new_v4().into_bytes());
    format!(
        "{prefix}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

async fn live_runtime() -> anyhow::Result<(Arc<UniverseRuntime>, sqlx::PgPool)> {
    let _ = dotenvy::dotenv();
    if std::env::var("LIGHTSPEED_TEST_POSTGRES_URL").is_err()
        && std::env::var("LIGHTSPEED_POSTGRES_URL").is_err()
    {
        anyhow::bail!(
            "LIGHTSPEED_TEST_POSTGRES_URL or LIGHTSPEED_POSTGRES_URL must be set; run ./dev.sh infra and source scripts/dev/env.sh"
        );
    }
    let temporal_target =
        std::env::var("TEMPORAL_ADDRESS").unwrap_or_else(|_| DEFAULT_TEMPORAL_TARGET.to_owned());
    let namespace = std::env::var("TEMPORAL_NAMESPACE")
        .unwrap_or_else(|_| DEFAULT_TEMPORAL_NAMESPACE.to_owned());
    let stores = DeploymentStores::from_env().await?;
    let pool = stores.pool().clone();
    let runtime = Arc::new(UniverseRuntime::new(
        connect_temporal(&temporal_target, &namespace).await?,
        format!("declared-keys-live-{}", Uuid::new_v4().simple()),
        Some("http://127.0.0.1:18080".to_owned()),
        stores,
    )?);
    Ok((runtime, pool))
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn a_declared_api_key_answers_to_the_supplied_secret() -> anyhow::Result<()> {
    let (runtime, pool) = live_runtime().await?;
    let operator = GatewayDeploymentApi::new(runtime);
    let keys = PgApiKeyStore::new(pool.clone());
    let universe_id = Uuid::new_v4();
    let scope = AccessScope::Universe { universe_id };
    let key_id = format!("release-bot-{}", Uuid::new_v4().simple());

    let result = async {
        calling(
            deployment_caller(),
            operator.create_universe(DeploymentUniverseCreateParams {
                universe_id: universe_id.to_string(),
                handle: None,
            }),
        )
        .await?;
        let declare = |key_id: &str, groups: &[MethodGroup], assert_actor, secret: &str| {
            calling(
                deployment_caller(),
                operator.put_api_key(DeploymentApiKeyPutParams {
                    scope,
                    key_id: key_id.to_owned(),
                    groups: Some(groups.to_vec()),
                    assert_actor,
                    display_name: "Release bot".to_owned(),
                    secret: secret.to_owned(),
                }),
            )
        };
        // What the gateway makes of a request bearing `secret`.
        let reaches = async |secret: &str, method: &str| {
            let mut headers = HeaderMap::new();
            headers.insert("authorization", format!("Bearer {secret}").parse().unwrap());
            authenticate(&keys, &headers, method, 1).await
        };
        let first = secret("lsk_", 1);
        let second = secret("lsk_", 2);

        // Declared once it exists and works; declared again nothing changes.
        let put = declare(&key_id, &[MethodGroup::Session], false, &first)
            .await?
            .result;
        assert!(put.created && !put.secret_replaced);
        assert_eq!(put.api_key.key_id, key_id);
        assert_eq!(put.api_key.key_prefix, &first[..12]);
        assert_eq!(reaches(&first, "session/read").await?.scope, scope);
        let put = declare(&key_id, &[MethodGroup::Session], false, &first)
            .await?
            .result;
        assert!(!put.created && !put.secret_replaced);

        // Its groups follow the declaration, taking effect on the next request.
        assert_eq!(
            reaches(&first, "profiles/list").await.unwrap_err().kind,
            AgentApiErrorKind::Forbidden
        );
        let put = declare(
            &key_id,
            &[MethodGroup::Session, MethodGroup::Profiles],
            false,
            &first,
        )
        .await?
        .result;
        assert!(!put.created && !put.secret_replaced);
        assert_eq!(
            put.api_key.groups,
            [MethodGroup::Session, MethodGroup::Profiles]
        );
        reaches(&first, "profiles/list").await?;

        // A different secret replaces the first with no overlap.
        let put = declare(
            &key_id,
            &[MethodGroup::Session, MethodGroup::Profiles],
            false,
            &second,
        )
        .await?
        .result;
        assert!(!put.created && put.secret_replaced);
        assert_eq!(put.api_key.key_prefix, &second[..12]);
        assert_eq!(
            reaches(&first, "session/read").await.unwrap_err().kind,
            AgentApiErrorKind::Unauthenticated
        );
        reaches(&second, "session/read").await?;

        // Refused: a malformed secret or id, a changed actor flag, a group the
        // scope cannot hold, and a secret that is already another key's.
        for (id, groups, assert_actor, supplied, kind) in [
            (
                key_id.as_str(),
                vec![MethodGroup::Session],
                false,
                "lsk_password",
                AgentApiErrorKind::InvalidRequest,
            ),
            (
                "not a key id",
                vec![MethodGroup::Session],
                false,
                second.as_str(),
                AgentApiErrorKind::InvalidRequest,
            ),
            (
                key_id.as_str(),
                vec![MethodGroup::DeploymentUniverses],
                false,
                second.as_str(),
                AgentApiErrorKind::InvalidRequest,
            ),
            (
                key_id.as_str(),
                vec![MethodGroup::Session],
                true,
                second.as_str(),
                AgentApiErrorKind::Conflict,
            ),
            (
                "another-key",
                vec![MethodGroup::Session],
                false,
                second.as_str(),
                AgentApiErrorKind::Conflict,
            ),
        ] {
            let error = declare(id, &groups, assert_actor, supplied)
                .await
                .unwrap_err();
            assert_eq!(error.kind, kind, "{id} / {supplied}");
        }
        reaches(&second, "session/read").await?;

        // A generated key may be named too, and a taken name is a conflict.
        let create = |key_id: &str| {
            calling(
                deployment_caller(),
                operator.create_api_key(DeploymentApiKeyCreateParams {
                    scope,
                    key_id: Some(key_id.to_owned()),
                    groups: None,
                    assert_actor: false,
                    display_name: "Generated".to_owned(),
                }),
            )
        };
        assert_eq!(
            create(&key_id).await.unwrap_err().kind,
            AgentApiErrorKind::Conflict
        );
        let generated = create("generated").await?.result;
        assert_eq!(generated.api_key.key_id, "generated");
        reaches(&generated.secret, "session/read").await?;

        // Rotation and revocation address the key by scope and id.
        let rotated = calling(
            deployment_caller(),
            operator.rotate_api_key(DeploymentApiKeyRotateParams {
                scope,
                key_id: key_id.clone(),
            }),
        )
        .await?
        .result;
        assert_eq!(rotated.api_key.key_id, key_id);
        assert!(reaches(&second, "session/read").await.is_err());
        reaches(&rotated.secret, "session/read").await?;
        let unknown = calling(
            deployment_caller(),
            operator.rotate_api_key(DeploymentApiKeyRotateParams {
                scope: AccessScope::Deployment,
                key_id: key_id.clone(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(unknown.kind, AgentApiErrorKind::NotFound);
        calling(
            deployment_caller(),
            operator.revoke_api_key(DeploymentApiKeyRevokeParams {
                scope,
                key_id: key_id.clone(),
            }),
        )
        .await?;
        assert!(reaches(&rotated.secret, "session/read").await.is_err());
        // A revoked key is never restored by declaring it again.
        let error = declare(&key_id, &[MethodGroup::Session], false, &secret("lsk_", 3))
            .await
            .unwrap_err();
        assert_eq!(error.kind, AgentApiErrorKind::Conflict);
        anyhow::Ok(())
    }
    .await;

    let _ = store_pg::delete_universe(&pool, universe_id).await;
    result
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn a_declared_registration_key_admits_under_the_supplied_secret() -> anyhow::Result<()> {
    let (runtime, pool) = live_runtime().await?;
    let operator = GatewayDeploymentApi::new(runtime.clone());
    let universe_id = Uuid::new_v4();
    let caller = || RequestContext::local(AccessScope::Universe { universe_id });

    let result = async {
        calling(
            deployment_caller(),
            operator.create_universe(DeploymentUniverseCreateParams {
                universe_id: universe_id.to_string(),
                handle: None,
            }),
        )
        .await?;
        let state = runtime.state_for(universe_id, false).await?;
        let api = state.api.clone();
        let store = state.store.clone();
        let declare = |id: &str, name: &str, mode, max_active, secret: &str| {
            calling(
                caller(),
                api.put_environment_registration_key(EnvironmentRegistrationKeyPutParams {
                    registration_key_id: id.to_owned(),
                    display_name: name.to_owned(),
                    identity_mode: mode,
                    max_active_environments: max_active,
                    ephemeral_disconnect_grace_ms: None,
                    offline_close_after_ms: None,
                    declared_only: false,
                    expires_at_ms: None,
                    secret: EnvironmentRegistrationSecretView(secret.to_owned()),
                }),
            )
        };
        // The key a daemon presenting `secret` would be admitted by.
        let admits = async |secret: &str| {
            EnvironmentRegistrationKeyStore::resolve_registration_key(
                store.as_ref(),
                &auth::secret_sha256_hex(secret),
            )
            .await
            .map(|key| key.map(|key| key.registration_key_id.to_string()))
        };
        let persistent = EnvironmentIdentityModeView::Persistent;
        let first = secret("lsrk_", 1);
        let second = secret("lsrk_", 2);

        // Declared once it exists under the chosen id and admits by the
        // supplied secret; declared again nothing changes.
        let put = declare("build-pool", "Build pool", persistent, Some(2), &first)
            .await?
            .result;
        assert!(put.created && !put.secret_replaced);
        assert_eq!(put.registration_key.registration_key_id, "build-pool");
        assert_eq!(put.registration_key.key_prefix, &first[..12]);
        assert_eq!(admits(&first).await?.as_deref(), Some("build-pool"));
        let put = declare("build-pool", "Build pool", persistent, Some(2), &first)
            .await?
            .result;
        assert!(!put.created && !put.secret_replaced);
        assert_eq!(
            put.registration_key.updated_at_ms,
            put.registration_key.created_at_ms
        );

        // Its editable policy follows the declaration.
        let put = declare("build-pool", "Build fleet", persistent, None, &first)
            .await?
            .result;
        assert!(!put.created && !put.secret_replaced);
        assert_eq!(put.registration_key.display_name, "Build fleet");
        assert_eq!(put.registration_key.max_active_environments, None);

        // A different secret retires the first at once.
        let put = declare("build-pool", "Build fleet", persistent, None, &second)
            .await?
            .result;
        assert!(!put.created && put.secret_replaced);
        assert_eq!(put.registration_key.key_prefix, &second[..12]);
        assert_eq!(admits(&first).await?, None);
        assert_eq!(admits(&second).await?.as_deref(), Some("build-pool"));

        // Refused: a malformed secret, a changed identity mode, and a secret
        // that is or ever was a key's, this one's retired secret included.
        let ephemeral = EnvironmentIdentityModeView::Ephemeral;
        for (id, mode, supplied, kind) in [
            (
                "build-pool",
                persistent,
                "lsrk_password",
                AgentApiErrorKind::InvalidRequest,
            ),
            (
                "build-pool",
                ephemeral,
                second.as_str(),
                AgentApiErrorKind::Conflict,
            ),
            (
                "other-pool",
                persistent,
                second.as_str(),
                AgentApiErrorKind::Conflict,
            ),
            (
                "build-pool",
                persistent,
                first.as_str(),
                AgentApiErrorKind::Conflict,
            ),
        ] {
            let error = declare(id, "Build fleet", mode, None, supplied)
                .await
                .unwrap_err();
            assert_eq!(error.kind, kind, "{id} / {supplied}");
        }
        assert_eq!(admits(&second).await?.as_deref(), Some("build-pool"));

        // A minted key may be named, and a taken name is a conflict.
        let create = |id: &str| {
            calling(
                caller(),
                api.create_environment_registration_key(EnvironmentRegistrationKeyCreateParams {
                    registration_key_id: Some(id.to_owned()),
                    display_name: "Minted".to_owned(),
                    identity_mode: persistent,
                    max_active_environments: None,
                    ephemeral_disconnect_grace_ms: None,
                    offline_close_after_ms: None,
                    declared_only: false,
                    expires_at_ms: None,
                }),
            )
        };
        assert_eq!(
            create("build-pool").await.unwrap_err().kind,
            AgentApiErrorKind::Conflict
        );
        assert_eq!(
            create("minted-pool")
                .await?
                .result
                .registration_key
                .registration_key_id,
            "minted-pool"
        );

        // A revoked key is never restored by declaring it again.
        calling(
            caller(),
            api.revoke_environment_registration_key(EnvironmentRegistrationKeyRevokeParams {
                registration_key_id: "build-pool".to_owned(),
                close_environments: false,
            }),
        )
        .await?;
        let error = declare(
            "build-pool",
            "Build fleet",
            persistent,
            None,
            &secret("lsrk_", 3),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, AgentApiErrorKind::Conflict);
        let read = calling(
            caller(),
            api.read_environment_registration_key(EnvironmentRegistrationKeyReadParams {
                registration_key_id: "build-pool".to_owned(),
            }),
        )
        .await?
        .result
        .registration_key;
        assert_eq!(read.status, EnvironmentRegistrationKeyStatusView::Revoked);
        anyhow::Ok(())
    }
    .await;

    let _ = store_pg::delete_universe(&pool, universe_id).await;
    result
}
