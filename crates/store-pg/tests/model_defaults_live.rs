use futures_util::FutureExt as _;
use sqlx::{
    Executor as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{panic::AssertUnwindSafe, str::FromStr};
use store_pg::{PgStore, PgStoreConfig};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local LIGHTSPEED_TEST_POSTGRES_URL; isolated schema"]
async fn universe_defaults_are_revision_safe_and_isolated() {
    let database_url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")
        .expect("LIGHTSPEED_TEST_POSTGRES_URL must be set; run ./dev.sh infra and source scripts/dev/env.sh");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect to live Postgres");
    let schema = format!("lightspeed_model_defaults_test_{}", Uuid::new_v4().simple());
    admin
        .execute(format!("CREATE SCHEMA \"{schema}\"").as_str())
        .await
        .expect("create isolated schema");
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(
            PgConnectOptions::from_str(&database_url)
                .expect("parse Postgres URL")
                .options([("search_path", schema.as_str())]),
        )
        .await
        .expect("connect to isolated schema");
    let outcome = AssertUnwindSafe(exercise(&pool)).catch_unwind().await;
    pool.close().await;
    admin
        .execute(format!("DROP SCHEMA \"{schema}\" CASCADE").as_str())
        .await
        .expect("drop isolated schema");
    admin.close().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

async fn exercise(pool: &sqlx::PgPool) {
    use api::{ModelConfig, ModelDefaultSlot as Slot, ModelDefaultsPutParams as Put};
    use store_pg::ModelDefaultsStoreError;
    PgStore::migrate(pool).await.unwrap();
    let first = PgStore::new(pool.clone(), PgStoreConfig::new(Uuid::new_v4()));
    let second = PgStore::new(pool.clone(), PgStoreConfig::new(Uuid::new_v4()));
    first.ensure_universe().await.unwrap();
    second.ensure_universe().await.unwrap();
    assert_eq!(
        first.read_model_defaults().await.unwrap(),
        api::ModelDefaults::default()
    );
    let route = |provider: &str, kind: &str| ModelConfig {
        provider_id: provider.into(),
        api_kind: kind.into(),
        model: "private-model".into(),
    };
    let request = Put {
        slot: Slot::AgentRun,
        model: Some(route("anthropic", "anthropic:messages")),
        expected_revision: 0,
    };
    let (left, right) = tokio::join!(
        first.put_model_defaults(request.clone()),
        first.put_model_defaults(request)
    );
    let winner = match (left, right) {
        (Ok(value), Err(ModelDefaultsStoreError::Conflict { expected: 0 }))
        | (Err(ModelDefaultsStoreError::Conflict { expected: 0 }), Ok(value)) => value,
        other => panic!("exactly one first writer must win: {other:?}"),
    };
    assert_eq!(winner.revision, 1);
    let other = second
        .put_model_defaults(Put {
            slot: Slot::AgentRun,
            model: Some(route("local", "openai:completions")),
            expected_revision: 0,
        })
        .await
        .unwrap();
    let speech = first
        .put_model_defaults(Put {
            slot: Slot::SpeechToText,
            model: Some(route("speech", "openai:audio-transcriptions")),
            expected_revision: 1,
        })
        .await
        .unwrap();
    assert_eq!(speech.agent_run, winner.agent_run);
    assert!(speech.speech_to_text.is_some());
    assert_eq!(second.read_model_defaults().await.unwrap(), other);

    let stale = first
        .put_model_defaults(Put {
            slot: Slot::AgentRun,
            model: None,
            expected_revision: 1,
        })
        .await
        .unwrap_err();
    assert!(matches!(
        stale,
        ModelDefaultsStoreError::Conflict { expected: 1 }
    ));
    let invalid = first
        .put_model_defaults(Put {
            slot: Slot::AgentRun,
            model: speech.speech_to_text.clone(),
            expected_revision: 2,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(invalid, ModelDefaultsStoreError::Invalid(error) if error.kind == api::AgentApiErrorKind::InvalidRequest)
    );
    assert_eq!(first.read_model_defaults().await.unwrap(), speech);

    let cleared = first
        .put_model_defaults(Put {
            slot: Slot::AgentRun,
            model: None,
            expected_revision: 2,
        })
        .await
        .unwrap();
    assert_eq!(cleared.revision, 3);
    assert_eq!(cleared.agent_run, None);
    assert_eq!(cleared.speech_to_text, speech.speech_to_text);
    let retry_seed = first
        .put_model_defaults(Put {
            slot: Slot::AgentRun,
            model: winner.agent_run,
            expected_revision: 0,
        })
        .await
        .unwrap_err();
    assert!(matches!(
        retry_seed,
        ModelDefaultsStoreError::Conflict { expected: 0 }
    ));
    assert_eq!(first.read_model_defaults().await.unwrap(), cleared);
    sqlx::query("DELETE FROM universes WHERE universe_id = $1")
        .bind(first.config().universe_id)
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(
        first.read_model_defaults().await.unwrap(),
        api::ModelDefaults::default()
    );
    assert_eq!(second.read_model_defaults().await.unwrap(), other);
}
