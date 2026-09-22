//! Governed resources: the immutable anchor reserved before a resource exists,
//! its root's policy, and grants. One statement loads what a decision needs;
//! the decision itself is `access::authorize`.
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
fn resource_ref(kind: &str, id: String) -> Result<ResourceRef, AccessError> {
    Ok(match kind {
        "session" => ResourceRef::Session(id),
        "bot" => ResourceRef::Bot(id),
        "profile" => ResourceRef::Profile(id),
        other => return Err(error(format!("unknown resource kind {other}"))),
    })
}
fn visibility_name(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Universe => "universe",
        Visibility::Restricted => "restricted",
    }
}
fn permission_from_rank(rank: Option<i32>) -> Option<ResourcePermission> {
    match rank {
        Some(2) => Some(ResourcePermission::Write),
        Some(1) => Some(ResourcePermission::Read),
        _ => None,
    }
}

const ACCESS_COLUMNS: &str = "a.resource_kind, a.resource_id, a.created_by, a.controller, a.audience_root_kind, a.audience_root_id, a.bot_id, a.created_at_ms,
    p.owner_principal_id, p.visibility, p.updated_by, p.updated_at_ms";
const ACCESS_JOIN: &str = "FROM access_resources a LEFT JOIN access_resource_policies p
    ON p.universe_id=a.universe_id AND p.resource_kind=a.audience_root_kind AND p.resource_id=a.audience_root_id";

fn anchor_row(row: &sqlx::postgres::PgRow) -> Result<ResourceAnchor, AccessError> {
    Ok(ResourceAnchor {
        resource: resource_ref(
            row.try_get("resource_kind").map_err(error)?,
            row.try_get("resource_id").map_err(error)?,
        )?,
        created_by: serde_json::from_value(row.try_get("created_by").map_err(error)?)
            .map_err(error)?,
        controller: serde_json::from_value(row.try_get("controller").map_err(error)?)
            .map_err(error)?,
        audience_root: resource_ref(
            row.try_get("audience_root_kind").map_err(error)?,
            row.try_get("audience_root_id").map_err(error)?,
        )?,
        bot: row.try_get("bot_id").map_err(error)?,
        created_at_ms: u64::try_from(row.try_get::<i64, _>("created_at_ms").map_err(error)?)
            .map_err(error)?,
    })
}

fn policy_row(row: &sqlx::postgres::PgRow) -> Result<Option<ResourcePolicy>, AccessError> {
    let Some(owner) = row
        .try_get::<Option<Uuid>, _>("owner_principal_id")
        .map_err(error)?
    else {
        return Ok(None);
    };
    let visibility = match row
        .try_get::<String, _>("visibility")
        .map_err(error)?
        .as_str()
    {
        "universe" => Visibility::Universe,
        "restricted" => Visibility::Restricted,
        other => return Err(error(format!("unknown visibility {other}"))),
    };
    Ok(Some(ResourcePolicy {
        owner,
        visibility,
        updated_by: serde_json::from_value(row.try_get("updated_by").map_err(error)?)
            .map_err(error)?,
        updated_at_ms: u64::try_from(row.try_get::<i64, _>("updated_at_ms").map_err(error)?)
            .map_err(error)?,
    }))
}

impl PgAccessStore {
    /// The anchor alone, for existence and lineage checks.
    pub async fn anchor(
        &self,
        universe: Uuid,
        resource: &ResourceRef,
    ) -> Result<Option<ResourceAnchor>, AccessError> {
        let (kind, id) = key(resource);
        sqlx::query("SELECT a.* FROM access_resources a WHERE a.universe_id=$1 AND a.resource_kind=$2 AND a.resource_id=$3")
            .bind(universe).bind(kind).bind(id).fetch_optional(&self.pool).await.map_err(error)?
            .as_ref().map(anchor_row).transpose()
    }

    /// Everything one decision needs, in one statement: the anchor, its
    /// root's policy, and the caller's best grant on that root through its
    /// own subject and its group memberships. Internal callers hold no
    /// principal and therefore no grant.
    pub async fn resource_access(
        &self,
        universe: Uuid,
        principal: Option<Uuid>,
        resource: &ResourceRef,
    ) -> Result<Option<ResourceAccess>, AccessError> {
        let (kind, id) = key(resource);
        let sql = format!(
            "SELECT {ACCESS_COLUMNS},
                (SELECT max(CASE g.permission WHEN 'write' THEN 2 WHEN 'read' THEN 1 END)
                 FROM access_resource_grants g
                 WHERE g.universe_id=a.universe_id AND g.resource_kind=a.audience_root_kind AND g.resource_id=a.audience_root_id
                   AND ((g.subject_kind='principal' AND g.subject_id=$4)
                     OR (g.subject_kind='group' AND g.subject_id IN
                         (SELECT m.group_id FROM access_memberships m WHERE m.principal_id=$4)))) AS grant_rank
             {ACCESS_JOIN}
             WHERE a.universe_id=$1 AND a.resource_kind=$2 AND a.resource_id=$3"
        );
        let row = sqlx::query(&sql)
            .bind(universe)
            .bind(kind)
            .bind(id)
            .bind(principal)
            .fetch_optional(&self.pool)
            .await
            .map_err(error)?;
        row.as_ref()
            .map(|row| {
                Ok(ResourceAccess {
                    anchor: anchor_row(row)?,
                    policy: policy_row(row)?,
                    grant: permission_from_rank(row.try_get("grant_rank").map_err(error)?),
                })
            })
            .transpose()
    }

    /// One decision for one caller on one existing resource: the role, then
    /// the loaded access facts. `None` when the resource has no anchor, so the
    /// caller can distinguish "never admitted" from "hidden".
    pub async fn decide(
        &self,
        caller: Caller<'_>,
        action: UniverseAction,
        resource: &ResourceRef,
    ) -> Result<Option<Decision>, AccessError> {
        let (universe, principal) = match caller {
            Caller::Request(rights) => {
                let AccessScope::Universe { universe_id } = rights.scope else {
                    return Ok(Some(Decision::Forbidden));
                };
                (universe_id, Some(rights.principal.id))
            }
            Caller::Controller(context) => (context.universe_id, None),
        };
        Ok(self
            .resource_access(universe, principal, resource)
            .await?
            .map(|access| authorize(caller, action, Some(&access))))
    }

    /// Read-only action preview for an existing resource, decided by the same
    /// evaluator as the mutation path from one lookup.
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
        let Some(access) = self
            .resource_access(universe_id, Some(rights.principal.id), resource)
            .await?
        else {
            return Ok(Vec::new());
        };
        use UniverseAction::*;
        let candidates: &[UniverseAction] = match resource {
            ResourceRef::Session(_) => &[Read, ControlSession, StopSession, DeleteSession],
            ResourceRef::Bot(_) => &[Read, ManageBot, InvokeBot],
            ResourceRef::Profile(_) => &[Read, ManageProfile],
        };
        let caller = Caller::Request(rights);
        let mut actions = Vec::new();
        for &action in candidates {
            if authorize(caller, action, Some(&access)) != Decision::Allowed {
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
                        && self
                            .decide(caller, DeleteSession, &ResourceRef::Session(target))
                            .await?
                            != Some(Decision::Allowed)
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

    /// Content existence is separate from anchors, which can survive failed
    /// creation. Permission previews must not expose actions on those
    /// reservations or on deleted content.
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

    /// Reserve before creating content or starting a workflow. A principal
    /// creates a root and owns it; a bot's session and a delegated child join
    /// their controller's root, so a missing controller fails closed. Retries
    /// return the original facts; failed starts retain their reservation.
    pub async fn reserve_resource(
        &self,
        universe: Uuid,
        resource: &ResourceRef,
        created_by: &ActionActor,
        controller: &ResourceController,
        now_ms: u64,
    ) -> Result<ResourceAnchor, AccessError> {
        let (kind, id) = key(resource);
        if id.is_empty() || id.len() > 256 {
            return Err(AccessError::Invalid("invalid resource id".into()));
        }
        // Never adopt pre-existing content lacking a trusted anchor.
        if self.resource_exists(universe, resource).await?
            && self.anchor(universe, resource).await?.is_none()
        {
            return Err(AccessError::Denied);
        }
        let (root, bot, owner) = match controller {
            ResourceController::Principal(id) => (resource.clone(), None, Some(*id)),
            ResourceController::Bot(bot) => {
                let parent = self
                    .anchor(universe, &ResourceRef::Bot(bot.clone()))
                    .await?
                    .ok_or(AccessError::Denied)?;
                (parent.audience_root, Some(bot.clone()), None)
            }
            ResourceController::Session(session) => {
                let parent = self
                    .anchor(universe, &ResourceRef::Session(session.clone()))
                    .await?
                    .ok_or(AccessError::Denied)?;
                (parent.audience_root, parent.bot, None)
            }
        };
        let (root_kind, root_id) = key(&root);
        let now = i64::try_from(now_ms).map_err(error)?;
        let mut tx = self.pool.begin().await.map_err(error)?;
        let inserted = sqlx::query("INSERT INTO access_resources(universe_id,resource_kind,resource_id,created_by,controller,audience_root_kind,audience_root_id,bot_id,created_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT DO NOTHING")
            .bind(universe).bind(kind).bind(id).bind(json!(created_by)).bind(json!(controller))
            .bind(root_kind).bind(root_id).bind(&bot).bind(now)
            .execute(&mut *tx).await.map_err(error)?.rows_affected() == 1;
        if inserted && let Some(owner) = owner {
            sqlx::query("INSERT INTO access_resource_policies(universe_id,resource_kind,resource_id,owner_principal_id,visibility,updated_by,updated_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7)")
                .bind(universe).bind(kind).bind(id).bind(owner).bind(visibility_name(Visibility::Universe))
                .bind(json!(created_by)).bind(now)
                .execute(&mut *tx).await.map_err(error)?;
        }
        tx.commit().await.map_err(error)?;
        let stored = self
            .anchor(universe, resource)
            .await?
            .ok_or(AccessError::Denied)?;
        if &stored.controller != controller || &stored.created_by != created_by {
            return Err(AccessError::Denied);
        }
        Ok(stored)
    }
}
