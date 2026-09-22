//! Immutable creation/control facts. The owner and managing bot are copied from
//! the admitted controller at reservation, so every check reads one row. Session
//! provenance is deliberately not consulted.
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
    /// Read-only action preview for an existing resource, decided by the same
    /// role matrix and ownership rule as the mutation path from one lookup.
    pub async fn resource_actions(
        &self,
        rights: &EffectiveAccess,
        resource: &ResourceRef,
        session_delete_cascade: bool,
    ) -> Result<Vec<UniverseAction>, AccessError> {
        let AccessScope::Universe { universe_id } = rights.scope else {
            return Ok(Vec::new());
        };
        if !self.resource_exists(universe_id, resource).await? {
            return Ok(Vec::new());
        }
        use UniverseAction::*;
        let candidates: &[UniverseAction] = match resource {
            ResourceRef::Session(_) => &[Read, ControlSession, StopSession, DeleteSession],
            ResourceRef::Bot(_) => &[Read, ManageBot, InvokeBot],
            ResourceRef::Profile(_) => &[Read, ManageProfile],
        };
        let controlled = self
            .ownership(universe_id, resource)
            .await?
            .is_some_and(|ownership| ownership.controlled_by(rights));
        let mut actions = Vec::new();
        for &action in candidates {
            let permitted = match rights.universe_action(action) {
                RoleDecision::Allowed => true,
                RoleDecision::RequiresOwnership => controlled,
                RoleDecision::Denied => false,
            };
            if !permitted {
                continue;
            }
            // Cascade deletion needs every session of the retention tree.
            if action == DeleteSession
                && session_delete_cascade
                && let ResourceRef::Session(id) = resource
            {
                let mut cascade_permitted = true;
                for target in self.session_deletion_targets(universe_id, id).await? {
                    if &target != id
                        && !self
                            .resource_permitted(
                                rights,
                                DeleteSession,
                                Some(&ResourceRef::Session(target)),
                            )
                            .await?
                    {
                        cascade_permitted = false;
                        break;
                    }
                }
                if !cascade_permitted {
                    continue;
                }
            }
            actions.push(action);
        }
        Ok(actions)
    }

    /// Content existence is separate from ownership reservations, which can
    /// survive failed creation. Permission previews must not expose actions on
    /// those reservations or on deleted content.
    pub async fn resource_exists(
        &self,
        universe: Uuid,
        resource: &ResourceRef,
    ) -> Result<bool, AccessError> {
        let (_, id) = key(resource);
        match resource {
            ResourceRef::Session(_) => sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions WHERE universe_id=$1 AND session_id=$2)"),
            ResourceRef::Bot(_) => sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM bots WHERE universe_id=$1 AND bot_id=$2)"),
            ResourceRef::Profile(_) => sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM agent_profiles WHERE universe_id=$1 AND profile_id=$2)"),
        }.bind(universe).bind(id).fetch_one(&self.pool).await.map_err(error)
    }

    /// The retention tree determines cascade deletion targets. It is broader
    /// than controller lineage: a history fork can belong to another principal.
    pub async fn session_deletion_targets(
        &self,
        universe: Uuid,
        session: &str,
    ) -> Result<Vec<String>, AccessError> {
        sqlx::query_scalar(
            "WITH RECURSIVE tree(session_id) AS (
            SELECT $2::text UNION SELECT child.session_id FROM sessions child JOIN tree parent
            ON (child.source_seq IS NOT NULL AND child.source_session_id=parent.session_id)
            OR child.origin_parent_session_id=parent.session_id WHERE child.universe_id=$1)
            SELECT session_id FROM tree",
        )
        .bind(universe)
        .bind(session)
        .fetch_all(&self.pool)
        .await
        .map_err(error)
    }

    pub async fn ownership(
        &self,
        universe: Uuid,
        resource: &ResourceRef,
    ) -> Result<Option<ResourceOwnership>, AccessError> {
        let (kind, id) = key(resource);
        let row = sqlx::query("SELECT owner_principal_id, bot_id, created_by, controller, created_at_ms FROM access_resource_ownership WHERE universe_id=$1 AND resource_kind=$2 AND resource_id=$3")
            .bind(universe).bind(kind).bind(id).fetch_optional(&self.pool).await.map_err(error)?;
        row.map(|row| {
            Ok(ResourceOwnership {
                resource: resource.clone(),
                created_by: serde_json::from_value(row.try_get("created_by").map_err(error)?)
                    .map_err(error)?,
                controller: serde_json::from_value(row.try_get("controller").map_err(error)?)
                    .map_err(error)?,
                owner: row.try_get("owner_principal_id").map_err(error)?,
                bot: row.try_get("bot_id").map_err(error)?,
                created_at_ms: u64::try_from(
                    row.try_get::<i64, _>("created_at_ms").map_err(error)?,
                )
                .map_err(error)?,
            })
        })
        .transpose()
    }

    /// Reserve before creating content or starting a workflow. The owner and
    /// managing bot come from the admitted controller's own immutable row, so a
    /// missing controller fails closed. Retries preserve the original actor and
    /// controller; failed starts retain their reservation.
    pub async fn reserve_ownership(
        &self,
        universe: Uuid,
        resource: &ResourceRef,
        created_by: &ActionActor,
        controller: &ResourceController,
        now_ms: u64,
    ) -> Result<ResourceOwnership, AccessError> {
        let (kind, id) = key(resource);
        if id.is_empty() || id.len() > 256 {
            return Err(AccessError::Invalid("invalid resource id".into()));
        }
        // Never adopt pre-existing content lacking trusted ownership.
        if self.resource_exists(universe, resource).await?
            && self.ownership(universe, resource).await?.is_none()
        {
            return Err(AccessError::Denied);
        }
        let (owner, bot) = match controller {
            ResourceController::Principal(id) => (*id, None),
            ResourceController::Bot(bot) => {
                let parent = self
                    .ownership(universe, &ResourceRef::Bot(bot.clone()))
                    .await?
                    .ok_or(AccessError::Denied)?;
                (parent.owner, Some(bot.clone()))
            }
            ResourceController::Session(session) => {
                let parent = self
                    .ownership(universe, &ResourceRef::Session(session.clone()))
                    .await?
                    .ok_or(AccessError::Denied)?;
                (parent.owner, parent.bot)
            }
        };
        sqlx::query("INSERT INTO access_resource_ownership(universe_id,resource_kind,resource_id,owner_principal_id,bot_id,controller,created_by,created_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT DO NOTHING")
            .bind(universe).bind(kind).bind(id).bind(owner).bind(&bot)
            .bind(json!(controller)).bind(json!(created_by))
            .bind(i64::try_from(now_ms).map_err(error)?)
            .execute(&self.pool).await.map_err(error)?;
        let stored = self
            .ownership(universe, resource)
            .await?
            .ok_or(AccessError::Denied)?;
        if &stored.controller != controller || &stored.created_by != created_by {
            return Err(AccessError::Denied);
        }
        Ok(stored)
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
        let (AccessScope::Universe { universe_id }, Some(resource)) = (rights.scope, resource)
        else {
            return Ok(false);
        };
        Ok(self
            .ownership(universe_id, resource)
            .await?
            .is_some_and(|ownership| ownership.controlled_by(rights)))
    }
}
