//! Immutable creation/control facts. Session provenance is deliberately not consulted.
use access::*;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::PgAccessStore;

fn error(error: impl std::fmt::Display) -> AccessError {
    AccessError::Store(error.to_string())
}
fn key(resource: &ResourceRef) -> (&'static str, &str) {
    match resource {
        ResourceRef::Session(id) => ("session", id),
        ResourceRef::Bot(id) => ("bot", id),
        ResourceRef::Profile(id) => ("profile", id),
    }
}

impl PgAccessStore {
    pub async fn ownership(
        &self,
        universe: Uuid,
        resource: &ResourceRef,
    ) -> Result<Option<ResourceOwnership>, AccessError> {
        let (kind, id) = key(resource);
        let row = sqlx::query("SELECT created_by, controller, created_at_ms FROM access_resource_ownership WHERE universe_id=$1 AND resource_kind=$2 AND resource_id=$3")
            .bind(universe).bind(kind).bind(id).fetch_optional(&self.pool).await.map_err(error)?;
        row.map(|row| {
            Ok(ResourceOwnership {
                resource: resource.clone(),
                created_by: serde_json::from_value(row.try_get("created_by").map_err(error)?)
                    .map_err(error)?,
                controller: serde_json::from_value(row.try_get("controller").map_err(error)?)
                    .map_err(error)?,
                created_at_ms: u64::try_from(
                    row.try_get::<i64, _>("created_at_ms").map_err(error)?,
                )
                .map_err(error)?,
            })
        })
        .transpose()
    }

    /// Reserve before creating content or starting a workflow. Retries preserve
    /// the original actor and controller. Failed starts retain their reservation.
    pub async fn reserve_ownership(
        &self,
        universe: Uuid,
        ownership: &ResourceOwnership,
    ) -> Result<(), AccessError> {
        let (kind, id) = key(&ownership.resource);
        if id.is_empty() || id.len() > 256 {
            return Err(AccessError::Invalid("invalid resource id".into()));
        }
        // Never adopt pre-existing content lacking trusted ownership.
        let content_exists: bool = match &ownership.resource {
            ResourceRef::Session(_) => sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions WHERE universe_id=$1 AND session_id=$2)"),
            ResourceRef::Bot(_) => sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM bots WHERE universe_id=$1 AND bot_id=$2)"),
            ResourceRef::Profile(_) => sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM agent_profiles WHERE universe_id=$1 AND profile_id=$2)"),
        }.bind(universe).bind(id).fetch_one(&self.pool).await.map_err(error)?;
        if content_exists
            && self
                .ownership(universe, &ownership.resource)
                .await?
                .is_none()
        {
            return Err(AccessError::Denied);
        }
        sqlx::query("INSERT INTO access_resource_ownership(universe_id,resource_kind,resource_id,created_by,controller,created_at_ms) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING")
            .bind(universe).bind(kind).bind(id)
            .bind(json!(ownership.created_by)).bind(json!(ownership.controller))
            .bind(i64::try_from(ownership.created_at_ms).map_err(error)?)
            .execute(&self.pool).await.map_err(error)?;
        let stored = self
            .ownership(universe, &ownership.resource)
            .await?
            .ok_or(AccessError::Denied)?;
        if stored.controller != ownership.controller || stored.created_by != ownership.created_by {
            return Err(AccessError::Denied);
        }
        Ok(())
    }

    /// Walk only explicitly admitted control edges. Missing facts, cycles and
    /// excessive depth fail closed; history forks never establish such edges.
    pub async fn controller_chain(
        &self,
        universe: Uuid,
        resource: &ResourceRef,
    ) -> Result<Vec<ResourceController>, AccessError> {
        let mut target = resource.clone();
        let mut seen = Vec::new();
        let mut chain = Vec::new();
        for _ in 0..64 {
            if seen.contains(&target) {
                return Err(AccessError::Denied);
            }
            seen.push(target.clone());
            let record = self
                .ownership(universe, &target)
                .await?
                .ok_or(AccessError::Denied)?;
            chain.push(record.controller.clone());
            match record.controller {
                ResourceController::Principal(_) => return Ok(chain),
                ResourceController::Bot(id) => target = ResourceRef::Bot(id),
                ResourceController::Session(id) => target = ResourceRef::Session(id),
            }
        }
        Err(AccessError::Denied)
    }

    pub async fn resource_permitted(
        &self,
        rights: &EffectiveAccess,
        action: UniverseAction,
        resource: Option<&ResourceRef>,
    ) -> Result<bool, AccessError> {
        match rights.universe_action(action) {
            RoleDecision::Allowed => return Ok(true),
            RoleDecision::Denied => return Ok(false),
            RoleDecision::RequiresOwnership => {}
        }
        let AccessScope::Universe { universe_id } = rights.scope else {
            return Ok(false);
        };
        let Some(resource) = resource else {
            return Ok(false);
        };
        let chain = match self.controller_chain(universe_id, resource).await {
            Ok(chain) => chain,
            Err(AccessError::Denied) => return Ok(false),
            Err(error) => return Err(error),
        };
        // Bot-controlled sessions follow bot management, including delegated
        // descendants. The operator role never widens a personal controller.
        if chain
            .iter()
            .any(|c| matches!(c, ResourceController::Bot(_)))
            && rights.universe_action(UniverseAction::ManageBot) == RoleDecision::Allowed
        {
            return Ok(true);
        }
        Ok(chain.last() == Some(&ResourceController::Principal(rights.principal.id)))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_action_admission(
        &self,
        universe: Uuid,
        actor: &ActionActor,
        authentication: Option<&RequestContext>,
        action: &str,
        resource: Option<&ResourceRef>,
        revision: Option<u64>,
        admitted: bool,
        now: u64,
    ) -> Result<(), AccessError> {
        let authentication = authentication.map(|c| {
            json!({
                "authenticatedPrincipal": c.authenticated_principal.id,
                "credential": c.authentication,
                "scope": c.credential_scope,
            })
        });
        sqlx::query("INSERT INTO access_action_audit(universe_id,actor,authentication,action,resource,policy_revision,admitted,occurred_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(universe).bind(json!(actor)).bind(authentication).bind(action)
            .bind(resource.map(|r| json!(r))).bind(revision.map(i64::try_from).transpose().map_err(error)?)
            .bind(admitted).bind(i64::try_from(now).map_err(error)?)
            .execute(&self.pool).await.map_err(error)?;
        Ok(())
    }
}
