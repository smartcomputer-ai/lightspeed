use api::{
    AgentProfile, AgentProfileInput, AgentProfileSummary, ProfileError, ProfileId, ProfileStore,
    UpdateAgentProfile,
};
use async_trait::async_trait;
use sqlx::Row;

use crate::PgStore;

#[async_trait]
impl ProfileStore for PgStore {
    async fn create_agent_profile(
        &self,
        profile: AgentProfileInput,
        created_at_ms: i64,
    ) -> Result<AgentProfile, ProfileError> {
        self.ensure_universe()
            .await
            .map_err(|error| profile_store_error("ensure universe", error))?;
        let record = profile.into_record(created_at_ms);
        record.validate()?;
        let document_json =
            serde_json::to_value(&record.document).map_err(|error| ProfileError::Store {
                message: format!("serialize profile document: {error}"),
            })?;
        let row = sqlx::query(
            r#"
            INSERT INTO agent_profiles (
                universe_id,
                profile_id,
                display_name,
                description,
                revision,
                document_json,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (universe_id, profile_id) DO NOTHING
            RETURNING
                profile_id,
                display_name,
                description,
                revision,
                document_json,
                created_at_ms,
                updated_at_ms
            "#,
        )
        .bind(self.config.universe_id)
        .bind(record.profile_id.as_str())
        .bind(record.display_name.as_deref())
        .bind(record.description.as_deref())
        .bind(u64_to_i64(record.revision, "revision")?)
        .bind(document_json)
        .bind(record.created_at_ms)
        .bind(record.updated_at_ms)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| profile_sql_error("create profile", error))?;

        let Some(row) = row else {
            return Err(ProfileError::AlreadyExists {
                profile_id: record.profile_id,
            });
        };
        profile_from_row(&row)
    }

    async fn read_agent_profile(
        &self,
        profile_id: &ProfileId,
    ) -> Result<AgentProfile, ProfileError> {
        let row = sqlx::query(
            r#"
            SELECT
                profile_id,
                display_name,
                description,
                revision,
                document_json,
                created_at_ms,
                updated_at_ms
            FROM agent_profiles
            WHERE universe_id = $1 AND profile_id = $2
            "#,
        )
        .bind(self.config.universe_id)
        .bind(profile_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| profile_sql_error("read profile", error))?;

        let Some(row) = row else {
            return Err(ProfileError::NotFound {
                profile_id: profile_id.clone(),
            });
        };
        profile_from_row(&row)
    }

    async fn list_agent_profiles(&self) -> Result<Vec<AgentProfileSummary>, ProfileError> {
        let rows = sqlx::query(
            r#"
            SELECT
                profile_id,
                display_name,
                description,
                revision,
                document_json,
                created_at_ms,
                updated_at_ms
            FROM agent_profiles
            WHERE universe_id = $1
            ORDER BY updated_at_ms DESC, profile_id
            "#,
        )
        .bind(self.config.universe_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| profile_sql_error("list profiles", error))?;

        rows.iter()
            .map(profile_from_row)
            .map(|result| result.map(|profile| profile.summary()))
            .collect()
    }

    async fn update_agent_profile(
        &self,
        update: UpdateAgentProfile,
    ) -> Result<AgentProfile, ProfileError> {
        self.ensure_universe()
            .await
            .map_err(|error| profile_store_error("ensure universe", error))?;
        let current = self.read_agent_profile(&update.profile_id).await?;
        if let Some(expected) = update.expected_revision
            && current.revision != expected
        {
            return Err(ProfileError::RevisionConflict {
                profile_id: update.profile_id,
                expected,
                actual: current.revision,
            });
        }
        let current_revision = current.revision;
        let profile_id = update.profile_id.clone();
        let updated = update.patch.apply_to(current, update.updated_at_ms)?;
        let document_json =
            serde_json::to_value(&updated.document).map_err(|error| ProfileError::Store {
                message: format!("serialize profile document: {error}"),
            })?;
        let row = sqlx::query(
            r#"
            UPDATE agent_profiles
            SET
                display_name = $3,
                description = $4,
                revision = $5,
                document_json = $6,
                updated_at_ms = $7
            WHERE universe_id = $1 AND profile_id = $2 AND revision = $8
            RETURNING
                profile_id,
                display_name,
                description,
                revision,
                document_json,
                created_at_ms,
                updated_at_ms
            "#,
        )
        .bind(self.config.universe_id)
        .bind(updated.profile_id.as_str())
        .bind(updated.display_name.as_deref())
        .bind(updated.description.as_deref())
        .bind(u64_to_i64(updated.revision, "revision")?)
        .bind(document_json)
        .bind(updated.updated_at_ms)
        .bind(u64_to_i64(current_revision, "revision")?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| profile_sql_error("update profile", error))?;

        let Some(row) = row else {
            let actual = self.read_agent_profile(&profile_id).await?.revision;
            return Err(ProfileError::RevisionConflict {
                profile_id,
                expected: current_revision,
                actual,
            });
        };
        profile_from_row(&row)
    }

    async fn delete_agent_profile(
        &self,
        profile_id: &ProfileId,
    ) -> Result<AgentProfile, ProfileError> {
        self.ensure_universe()
            .await
            .map_err(|error| profile_store_error("ensure universe", error))?;
        let row = sqlx::query(
            r#"
            DELETE FROM agent_profiles
            WHERE universe_id = $1 AND profile_id = $2
            RETURNING
                profile_id,
                display_name,
                description,
                revision,
                document_json,
                created_at_ms,
                updated_at_ms
            "#,
        )
        .bind(self.config.universe_id)
        .bind(profile_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| profile_sql_error("delete profile", error))?;

        let Some(row) = row else {
            return Err(ProfileError::NotFound {
                profile_id: profile_id.clone(),
            });
        };
        profile_from_row(&row)
    }
}

fn profile_from_row(row: &sqlx::postgres::PgRow) -> Result<AgentProfile, ProfileError> {
    let profile_id_value: String = row
        .try_get("profile_id")
        .map_err(|error| profile_sql_error("decode profile id", error))?;
    let profile_id = ProfileId::try_new(profile_id_value).map_err(|error| ProfileError::Store {
        message: format!("decode profile id: {error}"),
    })?;
    let revision: i64 = row
        .try_get("revision")
        .map_err(|error| profile_sql_error("decode profile revision", error))?;
    let document_json: serde_json::Value = row
        .try_get("document_json")
        .map_err(|error| profile_sql_error("decode profile document", error))?;
    let document = serde_json::from_value(document_json).map_err(|error| ProfileError::Store {
        message: format!("decode profile document: {error}"),
    })?;
    let profile = AgentProfile {
        profile_id,
        display_name: row
            .try_get("display_name")
            .map_err(|error| profile_sql_error("decode profile display_name", error))?,
        description: row
            .try_get("description")
            .map_err(|error| profile_sql_error("decode profile description", error))?,
        revision: i64_to_u64(revision, "revision")?,
        document,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|error| profile_sql_error("decode profile created_at_ms", error))?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|error| profile_sql_error("decode profile updated_at_ms", error))?,
    };
    profile.validate()?;
    Ok(profile)
}

fn u64_to_i64(value: u64, name: &'static str) -> Result<i64, ProfileError> {
    i64::try_from(value).map_err(|_| ProfileError::InvalidInput {
        message: format!("{name} exceeds i64::MAX"),
    })
}

fn i64_to_u64(value: i64, name: &'static str) -> Result<u64, ProfileError> {
    u64::try_from(value).map_err(|_| ProfileError::Store {
        message: format!("{name} is negative"),
    })
}

fn profile_store_error(action: &str, error: crate::PgStoreError) -> ProfileError {
    ProfileError::Store {
        message: format!("{action}: {error}"),
    }
}

fn profile_sql_error(action: &str, error: sqlx::Error) -> ProfileError {
    ProfileError::Store {
        message: format!("{action}: {error}"),
    }
}
