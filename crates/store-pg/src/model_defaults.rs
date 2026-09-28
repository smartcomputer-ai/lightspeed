use api::{ModelDefaultSlot, ModelDefaults, ModelDefaultsPutParams};
use sqlx::Row;
use thiserror::Error;

use crate::PgStore;

#[derive(Debug, Error)]
pub enum ModelDefaultsStoreError {
    #[error("model defaults revision conflict (expected {expected})")]
    Conflict { expected: u64 },
    #[error(transparent)]
    Invalid(#[from] api::AgentApiError),
    #[error("model defaults storage failure: {0}")]
    Postgres(#[from] sqlx::Error),
    #[error("invalid stored model defaults: {0}")]
    Decode(#[from] serde_json::Error),
}

impl PgStore {
    pub async fn read_model_defaults(&self) -> Result<ModelDefaults, ModelDefaultsStoreError> {
        let row = sqlx::query("SELECT revision, agent_run, speech_to_text FROM universe_model_defaults WHERE universe_id = $1")
            .bind(self.config.universe_id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref()
            .map(decode)
            .transpose()
            .map(Option::unwrap_or_default)
    }

    pub async fn put_model_defaults(
        &self,
        params: ModelDefaultsPutParams,
    ) -> Result<ModelDefaults, ModelDefaultsStoreError> {
        if let Some(model) = &params.model {
            params.slot.validate_model(model)?;
        }
        let expected = i64::try_from(params.expected_revision)
            .ok()
            .filter(|value| *value < i64::MAX)
            .ok_or_else(|| {
                api::AgentApiError::invalid_request("expectedRevision is out of range")
            })?;
        let model = params
            .model
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?;
        // An untouched universe uses a unique insert. Later writes use a
        // revision guard; neither path can overwrite a concurrent winner.
        let statement = if expected == 0 {
            "INSERT INTO universe_model_defaults (universe_id, revision, agent_run, speech_to_text)
             SELECT $1, 1, CASE WHEN $2 THEN $3::jsonb END, CASE WHEN NOT $2 THEN $3::jsonb END
             WHERE $4::bigint = 0
             ON CONFLICT (universe_id) DO NOTHING
             RETURNING revision, agent_run, speech_to_text"
        } else {
            "UPDATE universe_model_defaults SET revision = revision + 1,
             agent_run = CASE WHEN $2 THEN $3::jsonb ELSE agent_run END,
             speech_to_text = CASE WHEN NOT $2 THEN $3::jsonb ELSE speech_to_text END
             WHERE universe_id = $1 AND revision = $4
             RETURNING revision, agent_run, speech_to_text"
        };
        let row = sqlx::query(statement)
            .bind(self.config.universe_id)
            .bind(params.slot == ModelDefaultSlot::AgentRun)
            .bind(model)
            .bind(expected)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(ModelDefaultsStoreError::Conflict {
                expected: params.expected_revision,
            })?;
        decode(&row)
    }
}

fn decode(row: &sqlx::postgres::PgRow) -> Result<ModelDefaults, ModelDefaultsStoreError> {
    Ok(ModelDefaults {
        revision: row.try_get::<i64, _>("revision")? as u64,
        agent_run: row
            .try_get::<Option<serde_json::Value>, _>("agent_run")?
            .map(serde_json::from_value)
            .transpose()?,
        speech_to_text: row
            .try_get::<Option<serde_json::Value>, _>("speech_to_text")?
            .map(serde_json::from_value)
            .transpose()?,
    })
}
