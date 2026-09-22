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
pub(crate) fn key(resource: &ResourceRef) -> (&'static str, &str) {
    match resource {
        ResourceRef::Session(id) => ("session", id),
        ResourceRef::Bot(id) => ("bot", id),
        ResourceRef::Profile(id) => ("profile", id),
        ResourceRef::Collection(id) => ("collection", id),
    }
}
fn resource_ref(kind: &str, id: String) -> Result<ResourceRef, AccessError> {
    Ok(match kind {
        "session" => ResourceRef::Session(id),
        "bot" => ResourceRef::Bot(id),
        "profile" => ResourceRef::Profile(id),
        "collection" => ResourceRef::Collection(id),
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
    p.owner_principal_id, p.visibility, p.revision, p.updated_by, p.updated_at_ms";
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
        revision: u64::try_from(row.try_get::<i64, _>("revision").map_err(error)?)
            .map_err(error)?,
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
            ResourceRef::Session(_) => &[
                Read,
                ControlSession,
                StopSession,
                DeleteSession,
                ShareResource,
            ],
            ResourceRef::Bot(_) => &[Read, ManageBot, InvokeBot, ShareResource],
            ResourceRef::Profile(_) => &[Read, ManageProfile],
            ResourceRef::Collection(_) => &[
                Read,
                ControlSession,
                ManageCollection,
                DeleteCollection,
                ShareResource,
            ],
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
            ResourceRef::Collection(_) => sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM collections WHERE universe_id=$1 AND collection_id=$2)"),
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
    /// creates a root and owns it, or, naming a collection as `root`, creates
    /// a member of that collection; a bot's session and a delegated child join
    /// their controller's root, so a missing controller fails closed. Retries
    /// return the original facts; failed starts retain their reservation.
    pub async fn reserve_resource(
        &self,
        universe: Uuid,
        resource: &ResourceRef,
        created_by: &ActionActor,
        controller: &ResourceController,
        root: Option<&ResourceRef>,
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
        let (root, bot, owner) = match (controller, root) {
            (ResourceController::Principal(_), Some(collection @ ResourceRef::Collection(_))) => {
                // A member joins an existing collection and takes no policy.
                self.anchor(universe, collection)
                    .await?
                    .ok_or(AccessError::Denied)?;
                (collection.clone(), None, None)
            }
            (_, Some(_)) => {
                return Err(AccessError::Invalid(
                    "only a principal creates in a collection".into(),
                ));
            }
            (ResourceController::Principal(id), None) => (resource.clone(), None, Some(*id)),
            (ResourceController::Bot(bot), None) => {
                let parent = self
                    .anchor(universe, &ResourceRef::Bot(bot.clone()))
                    .await?
                    .ok_or(AccessError::Denied)?;
                (parent.audience_root, Some(bot.clone()), None)
            }
            (ResourceController::Session(session), None) => {
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

/// What a replacement of a root's policy says: the whole visibility and
/// grant set, guarded by the revision when given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyReplacement {
    pub visibility: Visibility,
    pub grants: Vec<(Subject, ResourcePermission)>,
    pub expected_revision: Option<u64>,
    /// Hand the root to this principal; the previous owner keeps nothing.
    pub owner: Option<Uuid>,
}

/// A root's policy and grants as one view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourcePolicyRecord {
    pub anchor: ResourceAnchor,
    pub policy: ResourcePolicy,
    pub grants: Vec<ResourceGrant>,
}

fn subject_key(subject: &Subject) -> (&'static str, Uuid) {
    match subject {
        Subject::Principal(id) => ("principal", *id),
        Subject::Group(id) => ("group", *id),
    }
}
fn permission_name(permission: ResourcePermission) -> &'static str {
    match permission {
        ResourcePermission::Read => "read",
        ResourcePermission::Write => "write",
    }
}

impl PgAccessStore {
    /// The policy of a resource's root with its grants; `None` when the
    /// resource has no anchor or its root no policy.
    pub async fn read_policy(
        &self,
        universe: Uuid,
        resource: &ResourceRef,
    ) -> Result<Option<ResourcePolicyRecord>, AccessError> {
        let Some(anchor) = self.anchor(universe, resource).await? else {
            return Ok(None);
        };
        let (root_kind, root_id) = key(&anchor.audience_root);
        let row = sqlx::query("SELECT owner_principal_id, visibility, revision, updated_by, updated_at_ms FROM access_resource_policies WHERE universe_id=$1 AND resource_kind=$2 AND resource_id=$3")
            .bind(universe).bind(root_kind).bind(root_id).fetch_optional(&self.pool).await.map_err(error)?;
        let Some(policy) = row.as_ref().map(policy_row).transpose()?.flatten() else {
            return Ok(None);
        };
        let grants = sqlx::query("SELECT subject_kind, subject_id, permission, granted_by, granted_at_ms FROM access_resource_grants WHERE universe_id=$1 AND resource_kind=$2 AND resource_id=$3 ORDER BY subject_kind, subject_id")
            .bind(universe).bind(root_kind).bind(root_id).fetch_all(&self.pool).await.map_err(error)?
            .iter().map(|row| {
                let id: Uuid = row.try_get("subject_id").map_err(error)?;
                let subject = match row.try_get::<String, _>("subject_kind").map_err(error)?.as_str() {
                    "principal" => Subject::Principal(id),
                    "group" => Subject::Group(id),
                    other => return Err(error(format!("unknown subject kind {other}"))),
                };
                let permission = match row.try_get::<String, _>("permission").map_err(error)?.as_str() {
                    "read" => ResourcePermission::Read,
                    "write" => ResourcePermission::Write,
                    other => return Err(error(format!("unknown permission {other}"))),
                };
                Ok(ResourceGrant {
                    subject,
                    permission,
                    granted_by: row.try_get("granted_by").map_err(error)?,
                    granted_at_ms: u64::try_from(row.try_get::<i64, _>("granted_at_ms").map_err(error)?).map_err(error)?,
                })
            }).collect::<Result<Vec<_>, AccessError>>()?;
        Ok(Some(ResourcePolicyRecord {
            anchor,
            policy,
            grants,
        }))
    }

    /// Replace a root's visibility and grant set, and hand it to a new owner
    /// when the replacement names one, in one transaction guarded by the
    /// policy revision when the caller supplies one. Every subject and a new
    /// owner must currently hold a role in the universe, directly or through
    /// a group; unchanged grants keep their attribution. The change is
    /// recorded and advances the deployment policy revision, so parked
    /// readers whose grant is gone revalidate and stop.
    pub async fn put_policy(
        &self,
        universe: Uuid,
        actor: Uuid,
        root: &ResourceRef,
        replacement: &PolicyReplacement,
        now_ms: u64,
    ) -> Result<ResourcePolicyRecord, AccessError> {
        let PolicyReplacement {
            visibility,
            grants,
            expected_revision,
            owner,
        } = replacement;
        let (visibility, expected_revision) = (*visibility, *expected_revision);
        let (root_kind, root_id) = key(root);
        let now = i64::try_from(now_ms).map_err(error)?;
        let mut tx = self.pool.begin().await.map_err(error)?;
        let current: Option<i64> = sqlx::query_scalar("SELECT revision FROM access_resource_policies WHERE universe_id=$1 AND resource_kind=$2 AND resource_id=$3 FOR UPDATE")
            .bind(universe).bind(root_kind).bind(root_id).fetch_optional(&mut *tx).await.map_err(error)?;
        let current = u64::try_from(current.ok_or(AccessError::NotFound)?).map_err(error)?;
        if let Some(expected) = expected_revision
            && expected != current
        {
            return Err(AccessError::RevisionMismatch {
                expected,
                actual: current,
            });
        }
        let mut seen = std::collections::BTreeSet::new();
        let subjects = grants
            .iter()
            .map(|(subject, _)| *subject)
            .chain(owner.map(Subject::Principal));
        for subject in subjects {
            if !seen.insert(subject) {
                return Err(AccessError::Invalid(format!(
                    "subject {subject:?} appears more than once"
                )));
            }
            let (subject_kind, subject_id) = subject_key(&subject);
            let member: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM access_role_assignments r WHERE r.universe_id=$1
                    AND (($2='principal' AND r.principal_id=$3) OR ($2='group' AND r.group_id=$3)))
                 OR EXISTS (SELECT 1 FROM access_memberships m JOIN access_role_assignments r ON r.group_id=m.group_id
                    WHERE $2='principal' AND m.principal_id=$3 AND r.universe_id=$1)",
            )
            .bind(universe)
            .bind(subject_kind)
            .bind(subject_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(error)?;
            if !member {
                return Err(AccessError::Invalid(format!(
                    "subject {subject:?} holds no role in this universe"
                )));
            }
        }
        sqlx::query("UPDATE access_resource_policies SET visibility=$4, owner_principal_id=COALESCE($7, owner_principal_id), revision=revision+1, updated_by=$5, updated_at_ms=$6 WHERE universe_id=$1 AND resource_kind=$2 AND resource_id=$3")
            .bind(universe).bind(root_kind).bind(root_id).bind(visibility_name(visibility))
            .bind(json!(ActionActor::Principal { id: actor })).bind(now).bind(owner)
            .execute(&mut *tx).await.map_err(error)?;
        let keep: Vec<String> = grants
            .iter()
            .map(|(subject, _)| {
                let (kind, id) = subject_key(subject);
                format!("{kind}:{id}")
            })
            .collect();
        sqlx::query("DELETE FROM access_resource_grants WHERE universe_id=$1 AND resource_kind=$2 AND resource_id=$3 AND NOT (subject_kind || ':' || subject_id::text = ANY($4))")
            .bind(universe).bind(root_kind).bind(root_id).bind(&keep)
            .execute(&mut *tx).await.map_err(error)?;
        for (subject, permission) in grants {
            let (subject_kind, subject_id) = subject_key(subject);
            sqlx::query("INSERT INTO access_resource_grants(universe_id,resource_kind,resource_id,subject_kind,subject_id,permission,granted_by,granted_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8)
                ON CONFLICT (universe_id,resource_kind,resource_id,subject_kind,subject_id) DO UPDATE
                SET permission=EXCLUDED.permission, granted_by=EXCLUDED.granted_by, granted_at_ms=EXCLUDED.granted_at_ms
                WHERE access_resource_grants.permission <> EXCLUDED.permission")
                .bind(universe).bind(root_kind).bind(root_id).bind(subject_kind).bind(subject_id)
                .bind(permission_name(*permission)).bind(actor).bind(now)
                .execute(&mut *tx).await.map_err(error)?;
        }
        crate::access::audit(
            &mut tx,
            Some(actor),
            json!({
                "operation": "put_resource_policy",
                "universeId": universe,
                "resource": root,
                "visibility": visibility,
                "owner": owner,
                "grants": grants.iter().map(|(subject, permission)| json!({"subject": subject, "permission": permission})).collect::<Vec<_>>(),
            }),
            now,
        )
        .await?;
        tx.commit().await.map_err(error)?;
        self.read_policy(universe, root)
            .await?
            .ok_or(AccessError::NotFound)
    }
}

/// Who a list is for. Lists return only what the reader may read, decided in
/// SQL by the same rule as a single read: universe-visible roots, the
/// reader's own roots, roots it holds a grant on, and, for internal work,
/// its own root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reader {
    /// Internal maintenance that sees everything; never a caller.
    Everything,
    Principal(Uuid),
    Root(ResourceRef),
}

impl Reader {
    pub fn from_caller(caller: Caller<'_>) -> Self {
        match caller {
            Caller::Request(rights) => Reader::Principal(rights.principal.id),
            Caller::Controller(context) => Reader::Root(context.root.clone()),
        }
    }

    /// The three bind values the predicate expects, in order.
    pub(crate) fn binds(&self) -> (Option<Uuid>, Option<&'static str>, Option<&str>) {
        match self {
            Reader::Everything => (None, None, None),
            Reader::Principal(id) => (Some(*id), None, None),
            Reader::Root(root) => {
                let (kind, id) = key(root);
                (None, Some(kind), Some(id))
            }
        }
    }
}

/// SQL predicate over a content row: `alias.id_column` names a resource of
/// `kind`; `$p` is the reader's principal, `$p+1`/`$p+2` its root. A row
/// without an anchored, policied root is never listed.
pub(crate) fn readable_predicate(
    reader: &Reader,
    kind: &str,
    alias: &str,
    id_column: &str,
    p: usize,
) -> String {
    if *reader == Reader::Everything {
        return "TRUE".to_owned();
    }
    let root_kind = p + 1;
    let root_id = p + 2;
    format!(
        "EXISTS (SELECT 1 FROM access_resources ra JOIN access_resource_policies rp
            ON rp.universe_id=ra.universe_id AND rp.resource_kind=ra.audience_root_kind AND rp.resource_id=ra.audience_root_id
          WHERE ra.universe_id={alias}.universe_id AND ra.resource_kind='{kind}' AND ra.resource_id={alias}.{id_column}
            AND (rp.visibility='universe'
              OR rp.owner_principal_id=${p}
              OR (ra.audience_root_kind=${root_kind} AND ra.audience_root_id=${root_id})
              OR EXISTS (SELECT 1 FROM access_resource_grants rg
                  WHERE rg.universe_id=ra.universe_id AND rg.resource_kind=ra.audience_root_kind AND rg.resource_id=ra.audience_root_id
                    AND ((rg.subject_kind='principal' AND rg.subject_id=${p})
                      OR (rg.subject_kind='group' AND rg.subject_id IN
                          (SELECT rm.group_id FROM access_memberships rm WHERE rm.principal_id=${p}))))))"
    )
}

/// Collections: a root with a name. Members are whatever anchors point at
/// it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionRecord {
    pub collection_id: String,
    pub display_name: String,
    pub revision: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

fn collection_row(row: &sqlx::postgres::PgRow) -> Result<CollectionRecord, AccessError> {
    Ok(CollectionRecord {
        collection_id: row.try_get("collection_id").map_err(error)?,
        display_name: row.try_get("display_name").map_err(error)?,
        revision: u64::try_from(row.try_get::<i64, _>("revision").map_err(error)?)
            .map_err(error)?,
        created_at_ms: u64::try_from(row.try_get::<i64, _>("created_at_ms").map_err(error)?)
            .map_err(error)?,
        updated_at_ms: u64::try_from(row.try_get::<i64, _>("updated_at_ms").map_err(error)?)
            .map_err(error)?,
    })
}

impl PgAccessStore {
    /// Create the collection row; the anchor and policy were reserved first.
    pub async fn create_collection(
        &self,
        universe: Uuid,
        collection_id: &str,
        display_name: &str,
        now_ms: u64,
    ) -> Result<CollectionRecord, AccessError> {
        let now = i64::try_from(now_ms).map_err(error)?;
        sqlx::query("INSERT INTO collections(universe_id,collection_id,display_name,created_at_ms,updated_at_ms) VALUES($1,$2,$3,$4,$4)")
            .bind(universe).bind(collection_id).bind(display_name).bind(now)
            .execute(&self.pool).await.map_err(error)?;
        self.read_collection(universe, collection_id)
            .await?
            .ok_or(AccessError::NotFound)
    }

    pub async fn read_collection(
        &self,
        universe: Uuid,
        collection_id: &str,
    ) -> Result<Option<CollectionRecord>, AccessError> {
        sqlx::query("SELECT * FROM collections WHERE universe_id=$1 AND collection_id=$2")
            .bind(universe)
            .bind(collection_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(error)?
            .as_ref()
            .map(collection_row)
            .transpose()
    }

    /// Collections the reader may read, by id.
    pub async fn list_collections(
        &self,
        universe: Uuid,
        reader: &Reader,
    ) -> Result<Vec<CollectionRecord>, AccessError> {
        let readable = readable_predicate(reader, "collection", "c", "collection_id", 2);
        let (principal, root_kind, root_id) = reader.binds();
        sqlx::query(&format!(
            "SELECT c.* FROM collections c WHERE c.universe_id=$1 AND {readable} ORDER BY c.collection_id"
        ))
        .bind(universe)
        .bind(principal)
        .bind(root_kind)
        .bind(root_id)
        .fetch_all(&self.pool)
        .await
        .map_err(error)?
        .iter()
        .map(collection_row)
        .collect()
    }

    /// Replace the name, guarded by the revision when given.
    pub async fn update_collection(
        &self,
        universe: Uuid,
        collection_id: &str,
        display_name: &str,
        expected_revision: Option<u64>,
        now_ms: u64,
    ) -> Result<CollectionRecord, AccessError> {
        let now = i64::try_from(now_ms).map_err(error)?;
        let mut tx = self.pool.begin().await.map_err(error)?;
        let current: i64 = sqlx::query_scalar(
            "SELECT revision FROM collections WHERE universe_id=$1 AND collection_id=$2 FOR UPDATE",
        )
        .bind(universe)
        .bind(collection_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(error)?
        .ok_or(AccessError::NotFound)?;
        let current = u64::try_from(current).map_err(error)?;
        if let Some(expected) = expected_revision
            && expected != current
        {
            return Err(AccessError::RevisionMismatch {
                expected,
                actual: current,
            });
        }
        sqlx::query("UPDATE collections SET display_name=$3, revision=revision+1, updated_at_ms=$4 WHERE universe_id=$1 AND collection_id=$2")
            .bind(universe).bind(collection_id).bind(display_name).bind(now)
            .execute(&mut *tx).await.map_err(error)?;
        tx.commit().await.map_err(error)?;
        self.read_collection(universe, collection_id)
            .await?
            .ok_or(AccessError::NotFound)
    }

    /// Resources whose audience root is this collection, other than itself.
    pub async fn collection_members(
        &self,
        universe: Uuid,
        collection_id: &str,
    ) -> Result<Vec<ResourceRef>, AccessError> {
        sqlx::query("SELECT resource_kind, resource_id FROM access_resources WHERE universe_id=$1 AND audience_root_kind='collection' AND audience_root_id=$2 AND NOT (resource_kind='collection' AND resource_id=$2) ORDER BY resource_kind, resource_id")
            .bind(universe).bind(collection_id).fetch_all(&self.pool).await.map_err(error)?
            .iter()
            .map(|row| resource_ref(row.try_get("resource_kind").map_err(error)?, row.try_get("resource_id").map_err(error)?))
            .collect()
    }

    /// Delete an empty collection with its anchor, policy and grants. A
    /// collection with members is refused: they are deleted first, so their
    /// own rules apply.
    pub async fn delete_collection(
        &self,
        universe: Uuid,
        collection_id: &str,
    ) -> Result<CollectionRecord, AccessError> {
        let record = self
            .read_collection(universe, collection_id)
            .await?
            .ok_or(AccessError::NotFound)?;
        let mut tx = self.pool.begin().await.map_err(error)?;
        let members: i64 = sqlx::query_scalar("SELECT count(*) FROM access_resources WHERE universe_id=$1 AND audience_root_kind='collection' AND audience_root_id=$2 AND NOT (resource_kind='collection' AND resource_id=$2)")
            .bind(universe).bind(collection_id).fetch_one(&mut *tx).await.map_err(error)?;
        if members > 0 {
            return Err(AccessError::Conflict);
        }
        sqlx::query("DELETE FROM collections WHERE universe_id=$1 AND collection_id=$2")
            .bind(universe)
            .bind(collection_id)
            .execute(&mut *tx)
            .await
            .map_err(error)?;
        sqlx::query("DELETE FROM access_resources WHERE universe_id=$1 AND resource_kind='collection' AND resource_id=$2")
            .bind(universe).bind(collection_id).execute(&mut *tx).await.map_err(error)?;
        tx.commit().await.map_err(error)?;
        Ok(record)
    }
}
