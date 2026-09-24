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
fn resource_ref(kind: &str, id: String) -> Result<ResourceRef, AccessError> {
    ResourceRef::from_kind(kind, id).ok_or_else(|| error(format!("unknown resource kind {kind}")))
}
/// When a use check runs, which decides what a resource whose record is
/// gone means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UseCheck {
    /// Admitting what a session or bot attaches: a missing resource is
    /// refused as missing.
    Admission,
    /// Continuing work that attached it, at run admission and every model
    /// call: a resource whose record no longer exists grants nothing and is
    /// skipped, since deleting a resource is not a revocation.
    Continuation,
}

/// Why the execution identity of a session or bot may not use what it
/// attaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UseRefusal {
    /// The root has no execution identity, or its identity is disabled or
    /// no longer eligible to use resources.
    Identity,
    /// The identity may not use `resource`: `Hidden` when it may not see it
    /// or it does not exist, `Forbidden` when it sees it.
    Resource {
        execution: Execution,
        resource: ResourceRef,
        decision: Decision,
    },
}

impl UseRefusal {
    /// Whether the identity itself may see what it was refused: false for a
    /// resource hidden from it or missing.
    pub fn visible_to_identity(&self) -> bool {
        !matches!(
            self,
            Self::Resource {
                decision: Decision::Hidden,
                ..
            }
        )
    }
}

/// The content row whose existence the anchor of `resource` stands for.
fn content_table(resource: &ResourceRef) -> (&'static str, &'static str) {
    match resource {
        ResourceRef::Session(_) => ("sessions", "session_id"),
        ResourceRef::Bot(_) => ("bots", "bot_id"),
        ResourceRef::Profile(_) => ("agent_profiles", "profile_id"),
        ResourceRef::Workspace(_) => ("vfs_workspaces", "workspace_id"),
        ResourceRef::Environment(_) => ("environments", "environment_id"),
        ResourceRef::McpServer(_) => ("mcp_servers", "server_id"),
    }
}
/// Whether the content row named by the SQL expressions `kind` and `id`
/// exists in universe `$1`, for any kind.
fn record_exists_sql(kind: &str, id: &str) -> String {
    let arms: Vec<String> = [
        ResourceRef::Session(String::new()),
        ResourceRef::Bot(String::new()),
        ResourceRef::Profile(String::new()),
        ResourceRef::Workspace(String::new()),
        ResourceRef::Environment(String::new()),
        ResourceRef::McpServer(String::new()),
    ]
    .iter()
    .map(|resource| {
        let (table, column) = content_table(resource);
        format!(
            "WHEN '{}' THEN EXISTS (SELECT 1 FROM {table} c WHERE c.universe_id=$1 AND c.{column}={id})",
            resource.kind()
        )
    })
    .collect();
    format!("CASE {kind} {} ELSE FALSE END", arms.join(" "))
}
fn visibility_name(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Universe => "universe",
        Visibility::Restricted => "restricted",
    }
}
fn visibility_from_name(name: &str) -> Result<Visibility, AccessError> {
    match name {
        "universe" => Ok(Visibility::Universe),
        "restricted" => Ok(Visibility::Restricted),
        other => Err(error(format!("unknown visibility {other}"))),
    }
}
fn execution_kind_name(kind: ExecutionKind) -> &'static str {
    match kind {
        ExecutionKind::Service => "service",
        ExecutionKind::Personal => "personal",
    }
}
fn execution_from(
    run_as: Option<Uuid>,
    kind: Option<String>,
) -> Result<Option<Execution>, AccessError> {
    match (run_as, kind.as_deref()) {
        (Some(run_as), Some("service")) => Ok(Some(Execution {
            run_as,
            kind: ExecutionKind::Service,
        })),
        (Some(run_as), Some("personal")) => Ok(Some(Execution {
            run_as,
            kind: ExecutionKind::Personal,
        })),
        (None, None) => Ok(None),
        other => Err(error(format!("inconsistent execution columns {other:?}"))),
    }
}
fn execution_from_row(row: &sqlx::postgres::PgRow) -> Result<Option<Execution>, AccessError> {
    execution_from(
        row.try_get("run_as_principal_id").map_err(error)?,
        row.try_get("execution_kind").map_err(error)?,
    )
}
fn permission_from_name(name: &str) -> Result<ResourcePermission, AccessError> {
    match name {
        "read" => Ok(ResourcePermission::Read),
        "write" => Ok(ResourcePermission::Write),
        "use" => Ok(ResourcePermission::Use),
        other => Err(error(format!("unknown permission {other}"))),
    }
}
/// Grants rank so that the best of several is one `max`. Each kind has one
/// grant permission or an ordered pair, so `use` never meets another.
fn permission_from_rank(rank: Option<i32>) -> Option<ResourcePermission> {
    match rank {
        Some(3) => Some(ResourcePermission::Use),
        Some(2) => Some(ResourcePermission::Write),
        Some(1) => Some(ResourcePermission::Read),
        _ => None,
    }
}
/// The rank of the best grant principal `$p` holds on the root named by
/// `kind_column`/`id_column` of `alias`, directly or through a group.
fn grant_rank_sql(alias: &str, kind_column: &str, id_column: &str, p: usize) -> String {
    format!(
        "(SELECT max(CASE g.permission WHEN 'use' THEN 3 WHEN 'write' THEN 2 WHEN 'read' THEN 1 END)
          FROM access_resource_grants g
          WHERE g.universe_id={alias}.universe_id AND g.resource_kind={alias}.{kind_column} AND g.resource_id={alias}.{id_column}
            AND ((g.subject_kind='principal' AND g.subject_id=${p})
              OR (g.subject_kind='group' AND g.subject_id IN
                  (SELECT m.group_id FROM access_memberships m WHERE m.principal_id=${p}))))"
    )
}

const ACCESS_COLUMNS: &str = "a.resource_kind, a.resource_id, a.created_by, a.controller, a.audience_root_kind, a.audience_root_id, a.bot_id, a.run_as_principal_id, a.execution_kind, a.created_at_ms,
    p.owner_principal_id, p.visibility, p.revision, p.updated_by, p.updated_at_ms";
const ACCESS_JOIN: &str = "FROM access_resources a LEFT JOIN access_resource_policies p
    ON p.universe_id=a.universe_id AND p.resource_kind=a.audience_root_kind AND p.resource_id=a.audience_root_id";

fn anchor_row(row: &sqlx::postgres::PgRow) -> Result<ResourceAnchor, AccessError> {
    Ok(ResourceAnchor {
        resource: resource_ref(
            &row.try_get::<String, _>("resource_kind").map_err(error)?,
            row.try_get("resource_id").map_err(error)?,
        )?,
        created_by: serde_json::from_value(row.try_get("created_by").map_err(error)?)
            .map_err(error)?,
        controller: serde_json::from_value(row.try_get("controller").map_err(error)?)
            .map_err(error)?,
        audience_root: resource_ref(
            &row.try_get::<String, _>("audience_root_kind")
                .map_err(error)?,
            row.try_get("audience_root_id").map_err(error)?,
        )?,
        bot: row.try_get("bot_id").map_err(error)?,
        execution: execution_from_row(row)?,
        created_at_ms: u64::try_from(row.try_get::<i64, _>("created_at_ms").map_err(error)?)
            .map_err(error)?,
    })
}

/// The summary a view carries; `None` when the resource has no anchor or its
/// root no policy, which a list never returns and a read hides.
fn summary_row(row: &sqlx::postgres::PgRow) -> Result<Option<ResourceAccessSummary>, AccessError> {
    Ok(ResourceAccess {
        anchor: anchor_row(row)?,
        policy: policy_row(row)?,
        grant: None,
    }
    .summary())
}

/// The access summary columns of a content row's anchor, aliased so they
/// never collide with the content row's own, for list queries that join the
/// anchor and its root's policy as `ra`/`rp`.
pub(crate) const SUMMARY_COLUMNS: &str =
    "ra.audience_root_kind AS access_root_kind, ra.audience_root_id AS access_root_id,
    ra.run_as_principal_id AS access_run_as, ra.execution_kind AS access_execution_kind,
    rp.owner_principal_id AS access_owner, rp.visibility AS access_visibility";

pub(crate) fn summary_from_list_row(
    row: &sqlx::postgres::PgRow,
) -> Result<ResourceAccessSummary, AccessError> {
    Ok(ResourceAccessSummary {
        root: resource_ref(
            &row.try_get::<String, _>("access_root_kind")
                .map_err(error)?,
            row.try_get("access_root_id").map_err(error)?,
        )?,
        owner: row.try_get("access_owner").map_err(error)?,
        visibility: visibility_from_name(
            &row.try_get::<String, _>("access_visibility")
                .map_err(error)?,
        )?,
        execution: execution_from(
            row.try_get("access_run_as").map_err(error)?,
            row.try_get("access_execution_kind").map_err(error)?,
        )?,
    })
}

/// The joins that attach a content row's anchor and its root's policy as
/// `ra`/`rp`, for list queries selecting `SUMMARY_COLUMNS`. An inner join:
/// a row without an anchored, policied root is never listed.
pub(crate) fn summary_join(kind: &str, alias: &str, id_column: &str) -> String {
    format!(
        "JOIN access_resources ra ON ra.universe_id = {alias}.universe_id AND ra.resource_kind = '{kind}' AND ra.resource_id = {alias}.{id_column}
         JOIN access_resource_policies rp ON rp.universe_id = ra.universe_id AND rp.resource_kind = ra.audience_root_kind AND rp.resource_id = ra.audience_root_id"
    )
}

/// `a, b` as `alias.a, alias.b`, for column lists that meet the anchor's
/// columns of the same names.
pub(crate) fn qualify_columns(columns: &str, alias: &str) -> String {
    columns
        .split(',')
        .map(str::trim)
        .filter(|column| !column.is_empty())
        .map(|column| format!("{alias}.{column}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn policy_row(row: &sqlx::postgres::PgRow) -> Result<Option<ResourcePolicy>, AccessError> {
    let Some(owner) = row
        .try_get::<Option<Uuid>, _>("owner_principal_id")
        .map_err(error)?
    else {
        return Ok(None);
    };
    let visibility = visibility_from_name(&row.try_get::<String, _>("visibility").map_err(error)?)?;
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

/// The anchor of a resource, loaded through any executor so a transaction
/// can read it under its locks.
async fn load_anchor<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    universe: Uuid,
    resource: &ResourceRef,
) -> Result<Option<ResourceAnchor>, AccessError> {
    sqlx::query("SELECT a.* FROM access_resources a WHERE a.universe_id=$1 AND a.resource_kind=$2 AND a.resource_id=$3")
        .bind(universe).bind(resource.kind()).bind(resource.id()).fetch_optional(executor).await.map_err(error)?
        .as_ref().map(anchor_row).transpose()
}

/// Every grant on a root, in a stable order.
async fn load_grants<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    universe: Uuid,
    root: &ResourceRef,
) -> Result<Vec<ResourceGrant>, AccessError> {
    sqlx::query("SELECT subject_kind, subject_id, permission, granted_by, granted_at_ms FROM access_resource_grants WHERE universe_id=$1 AND resource_kind=$2 AND resource_id=$3 ORDER BY subject_kind, subject_id")
        .bind(universe).bind(root.kind()).bind(root.id()).fetch_all(executor).await.map_err(error)?
        .iter().map(|row| {
            let id: Uuid = row.try_get("subject_id").map_err(error)?;
            let subject = match row.try_get::<String, _>("subject_kind").map_err(error)?.as_str() {
                "principal" => Subject::Principal(id),
                "group" => Subject::Group(id),
                other => return Err(error(format!("unknown subject kind {other}"))),
            };
            Ok(ResourceGrant {
                subject,
                permission: permission_from_name(&row.try_get::<String, _>("permission").map_err(error)?)?,
                granted_by: row.try_get("granted_by").map_err(error)?,
                granted_at_ms: u64::try_from(row.try_get::<i64, _>("granted_at_ms").map_err(error)?).map_err(error)?,
            })
        }).collect()
}

impl PgAccessStore {
    /// The anchor alone, for existence and lineage checks.
    pub async fn anchor(
        &self,
        universe: Uuid,
        resource: &ResourceRef,
    ) -> Result<Option<ResourceAnchor>, AccessError> {
        load_anchor(&self.pool, universe, resource).await
    }

    /// The summary a view carries, resolved through the root.
    pub async fn access_summary(
        &self,
        universe: Uuid,
        resource: &ResourceRef,
    ) -> Result<Option<ResourceAccessSummary>, AccessError> {
        let sql = format!(
            "SELECT {ACCESS_COLUMNS} {ACCESS_JOIN} WHERE a.universe_id=$1 AND a.resource_kind=$2 AND a.resource_id=$3"
        );
        sqlx::query(&sql)
            .bind(universe)
            .bind(resource.kind())
            .bind(resource.id())
            .fetch_optional(&self.pool)
            .await
            .map_err(error)?
            .as_ref()
            .map(summary_row)
            .transpose()
            .map(Option::flatten)
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
        let grant_rank = grant_rank_sql("a", "audience_root_kind", "audience_root_id", 4);
        let sql = format!(
            "SELECT {ACCESS_COLUMNS}, {grant_rank} AS grant_rank
             {ACCESS_JOIN}
             WHERE a.universe_id=$1 AND a.resource_kind=$2 AND a.resource_id=$3"
        );
        let row = sqlx::query(&sql)
            .bind(universe)
            .bind(resource.kind())
            .bind(resource.id())
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
    /// the loaded access facts. `None` when a session, bot or profile has no
    /// anchor, so the caller can distinguish "never admitted" from "hidden".
    /// A workspace, environment or MCP server is anchored before it exists
    /// and is never named by its own creation, so a missing anchor hides it;
    /// internal work is decided on one as its execution principal, whose
    /// rights and grants are what a session uses it with.
    pub async fn decide(
        &self,
        caller: Caller<'_>,
        action: UniverseAction,
        resource: &ResourceRef,
    ) -> Result<Option<Decision>, AccessError> {
        Ok(self.decide_with_access(caller, action, resource).await?.0)
    }

    /// `decide`, with the access facts it read when the resource has an
    /// anchor, so a view carries its access summary without a second
    /// lookup.
    pub async fn decide_with_access(
        &self,
        caller: Caller<'_>,
        action: UniverseAction,
        resource: &ResourceRef,
    ) -> Result<(Option<Decision>, Option<ResourceAccess>), AccessError> {
        let (universe, principal) = match caller {
            Caller::Request(rights) => {
                let AccessScope::Universe { universe_id } = rights.scope else {
                    return Ok((Some(Decision::Forbidden), None));
                };
                (universe_id, Some(rights.principal.id))
            }
            Caller::Controller(context) => (context.universe_id, None),
        };
        if !resource.is_operational() {
            let access = self.resource_access(universe, principal, resource).await?;
            let decision = access
                .as_ref()
                .map(|access| authorize(caller, action, Some(access)));
            return Ok((decision, access));
        }
        let loaded;
        let rights = match caller {
            Caller::Request(rights) => rights,
            Caller::Controller(context) => {
                let Some(principal) = context.execution_principal else {
                    return Ok((Some(Decision::Forbidden), None));
                };
                let Some(rights) = self.universe_rights(universe, principal).await? else {
                    return Ok((Some(Decision::Forbidden), None));
                };
                loaded = rights;
                &loaded
            }
        };
        let access = self
            .resource_access(universe, Some(rights.principal.id), resource)
            .await?;
        let decision = access.as_ref().map_or(Decision::Hidden, |access| {
            authorize(Caller::Request(rights), action, Some(access))
        });
        Ok((Some(decision), access))
    }

    /// The execution identity a session or bot runs as, with that
    /// principal's current rights in the universe. `None` when the root has
    /// no anchor or no execution, or its principal no longer exists: such a
    /// root holds no authority.
    async fn execution_rights(
        &self,
        universe: Uuid,
        root: &ResourceRef,
    ) -> Result<Option<(Execution, EffectiveAccess)>, AccessError> {
        let Some(execution) = load_anchor(&self.pool, universe, root)
            .await?
            .and_then(|anchor| anchor.execution)
        else {
            return Ok(None);
        };
        Ok(self
            .universe_rights(universe, execution.run_as)
            .await?
            .map(|rights| (execution, rights)))
    }

    /// Whether the execution identity of `root`, a session or a bot, may
    /// use every one of `resources` now: the one check behind attachment
    /// admission, run admission, the per-turn check and bot triggers. The
    /// identity must be active and eligible to use resources, then each
    /// resource is decided for it in one statement, in order, the first
    /// refusal reported. Admission of nothing admits nothing and reads no
    /// identity.
    pub async fn execution_use(
        &self,
        universe: Uuid,
        root: &ResourceRef,
        resources: &[ResourceRef],
        check: UseCheck,
    ) -> Result<Option<UseRefusal>, AccessError> {
        if resources.is_empty() && check == UseCheck::Admission {
            return Ok(None);
        }
        let Some((execution, rights)) = self.execution_rights(universe, root).await? else {
            return Ok(Some(UseRefusal::Identity));
        };
        if rights.universe_action(UniverseAction::UseResource) != RoleDecision::Allowed {
            return Ok(Some(UseRefusal::Identity));
        }
        if resources.is_empty() {
            return Ok(None);
        }
        let mut considered = Vec::with_capacity(resources.len());
        let mut accesses = Vec::with_capacity(resources.len());
        for (resource, exists, access) in self
            .use_facts(universe, rights.principal.id, resources)
            .await?
        {
            match (exists, check) {
                // A deleted resource grants nothing and revokes nothing:
                // work that attached it continues without it.
                (false, UseCheck::Continuation) => continue,
                // Attaching what does not exist is refused as missing.
                (false, UseCheck::Admission) => {}
                // A record without an anchor has no access facts and is
                // hidden, failing closed.
                (true, _) => accesses.extend(access),
            }
            considered.push(resource);
        }
        Ok(
            access::first_unusable(&rights, &considered, &accesses).map(|(resource, decision)| {
                UseRefusal::Resource {
                    execution,
                    resource,
                    decision,
                }
            }),
        )
    }

    /// For each of `resources` in order: whether its record exists, and
    /// what `resource_access` loads for `principal` when it has an anchor.
    async fn use_facts(
        &self,
        universe: Uuid,
        principal: Uuid,
        resources: &[ResourceRef],
    ) -> Result<Vec<(ResourceRef, bool, Option<ResourceAccess>)>, AccessError> {
        let grant_rank = grant_rank_sql("a", "audience_root_kind", "audience_root_id", 4);
        let exists = record_exists_sql("r.kind", "r.id");
        let sql = format!(
            "SELECT r.kind AS requested_kind, r.id AS requested_id, {exists} AS record_exists,
                    {ACCESS_COLUMNS}, {grant_rank} AS grant_rank
             FROM unnest($2::text[], $3::text[]) WITH ORDINALITY AS r(kind, id, position)
             LEFT JOIN access_resources a ON a.universe_id=$1 AND a.resource_kind=r.kind AND a.resource_id=r.id
             LEFT JOIN access_resource_policies p
               ON p.universe_id=a.universe_id AND p.resource_kind=a.audience_root_kind AND p.resource_id=a.audience_root_id
             ORDER BY r.position"
        );
        let (kinds, ids): (Vec<&str>, Vec<&str>) = resources
            .iter()
            .map(|resource| (resource.kind(), resource.id()))
            .unzip();
        sqlx::query(&sql)
            .bind(universe)
            .bind(kinds)
            .bind(ids)
            .bind(principal)
            .fetch_all(&self.pool)
            .await
            .map_err(error)?
            .iter()
            .map(|row| {
                let resource = resource_ref(
                    &row.try_get::<String, _>("requested_kind").map_err(error)?,
                    row.try_get("requested_id").map_err(error)?,
                )?;
                let anchored = row
                    .try_get::<Option<String>, _>("resource_kind")
                    .map_err(error)?
                    .is_some();
                let access = anchored
                    .then(|| {
                        Ok::<_, AccessError>(ResourceAccess {
                            anchor: anchor_row(row)?,
                            policy: policy_row(row)?,
                            grant: permission_from_rank(row.try_get("grant_rank").map_err(error)?),
                        })
                    })
                    .transpose()?;
                Ok((
                    resource,
                    row.try_get("record_exists").map_err(error)?,
                    access,
                ))
            })
            .collect()
    }

    async fn universe_rights(
        &self,
        universe: Uuid,
        principal: Uuid,
    ) -> Result<Option<EffectiveAccess>, AccessError> {
        match self
            .effective_access(
                principal,
                AccessScope::Universe {
                    universe_id: universe,
                },
            )
            .await
        {
            Ok(rights) => Ok(Some(rights)),
            Err(AccessError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
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
            ResourceRef::Workspace(_) | ResourceRef::Environment(_) | ResourceRef::McpServer(_) => {
                &[Read, UseResource, ConfigureResource, ShareResource]
            }
        };
        let caller = Caller::Request(rights);
        let mut actions = Vec::new();
        for &action in candidates {
            if !authorize(caller, action, Some(&access)).allows() {
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
                            .decide(caller, DeleteSession, &ResourceRef::Session(target))
                            .await?
                            .is_some_and(Decision::allows)
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
        let (table, id_column) = content_table(resource);
        sqlx::query_scalar(&format!(
            "SELECT EXISTS(SELECT 1 FROM {table} WHERE universe_id=$1 AND {id_column}=$2)"
        ))
        .bind(universe)
        .bind(resource.id())
        .fetch_one(&self.pool)
        .await
        .map_err(error)
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
    /// creates a root and owns it, universe-visible unless `visibility` says
    /// otherwise; a bot's session and a delegated child join their
    /// controller's root, so a missing controller fails closed. Sessions and
    /// bots run work and need an execution identity; profiles, workspaces,
    /// environments and MCP servers run nothing and take none. Session and
    /// bot retries return the original facts; failed starts retain their
    /// reservation. A workspace, environment or MCP server id is reserved
    /// once: an existing anchor means the id is taken (`Conflict`), so a
    /// repeated create never touches a live resource's access.
    #[allow(clippy::too_many_arguments)]
    pub async fn reserve_resource(
        &self,
        universe: Uuid,
        resource: &ResourceRef,
        created_by: &ActionActor,
        controller: &ResourceController,
        execution: Option<Execution>,
        visibility: Option<Visibility>,
        now_ms: u64,
    ) -> Result<ResourceAnchor, AccessError> {
        let (kind, id) = (resource.kind(), resource.id());
        if id.is_empty() || id.len() > 256 {
            return Err(AccessError::Invalid("invalid resource id".into()));
        }
        if resource.is_operational() && !matches!(controller, ResourceController::Principal(_)) {
            return Err(AccessError::Invalid(format!(
                "a {kind} is a root created by a principal"
            )));
        }
        // Never adopt pre-existing content lacking a trusted anchor.
        if self.resource_exists(universe, resource).await?
            && self.anchor(universe, resource).await?.is_none()
        {
            return Err(AccessError::Denied);
        }
        // A root takes the execution it was given; everything below a root
        // copies its root's, so a child never carries its own.
        let (root, bot, owner, execution) = match controller {
            ResourceController::Principal(id) => {
                let runs_work = matches!(resource, ResourceRef::Session(_) | ResourceRef::Bot(_));
                if execution.is_some() != runs_work {
                    return Err(AccessError::Invalid(if runs_work {
                        "a root that runs work needs an execution identity".into()
                    } else {
                        format!("a {kind} runs nothing and takes no execution identity")
                    }));
                }
                (resource.clone(), None, Some(*id), execution)
            }
            ResourceController::Bot(bot) => {
                let parent = self
                    .anchor(universe, &ResourceRef::Bot(bot.clone()))
                    .await?
                    .ok_or(AccessError::Denied)?;
                (
                    parent.audience_root,
                    Some(bot.clone()),
                    None,
                    parent.execution,
                )
            }
            ResourceController::Session(session) => {
                let parent = self
                    .anchor(universe, &ResourceRef::Session(session.clone()))
                    .await?
                    .ok_or(AccessError::Denied)?;
                (parent.audience_root, parent.bot, None, parent.execution)
            }
        };
        if visibility.is_some() && owner.is_none() {
            return Err(AccessError::Invalid(
                "a resource created under another root has no audience of its own".into(),
            ));
        }
        let now = i64::try_from(now_ms).map_err(error)?;
        let mut tx = self.pool.begin().await.map_err(error)?;
        let inserted = sqlx::query("INSERT INTO access_resources(universe_id,resource_kind,resource_id,created_by,controller,audience_root_kind,audience_root_id,bot_id,run_as_principal_id,execution_kind,created_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT DO NOTHING")
            .bind(universe).bind(kind).bind(id).bind(json!(created_by)).bind(json!(controller))
            .bind(root.kind()).bind(root.id()).bind(&bot)
            .bind(execution.map(|execution| execution.run_as))
            .bind(execution.map(|execution| execution_kind_name(execution.kind)))
            .bind(now)
            .execute(&mut *tx).await.map_err(error)?.rows_affected() == 1;
        if !inserted && resource.is_operational() {
            return Err(AccessError::Conflict);
        }
        if inserted && let Some(owner) = owner {
            sqlx::query("INSERT INTO access_resource_policies(universe_id,resource_kind,resource_id,owner_principal_id,visibility,updated_by,updated_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7)")
                .bind(universe).bind(kind).bind(id).bind(owner)
                .bind(visibility_name(visibility.unwrap_or(Visibility::Universe)))
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

    /// Release the anchor of a resource whose creation did not happen: the
    /// create failed, or a retry resolved to an existing resource under
    /// another id. An anchor whose resource exists is kept, so a concurrent
    /// creation that succeeded never loses its access facts. Returns whether
    /// an anchor was released.
    pub async fn release_resource(
        &self,
        universe: Uuid,
        resource: &ResourceRef,
    ) -> Result<bool, AccessError> {
        let (table, id_column) = content_table(resource);
        let released = sqlx::query(&format!(
            "DELETE FROM access_resources a WHERE a.universe_id=$1 AND a.resource_kind=$2 AND a.resource_id=$3
               AND NOT EXISTS (SELECT 1 FROM {table} c WHERE c.universe_id=$1 AND c.{id_column}=$3)"
        ))
        .bind(universe)
        .bind(resource.kind())
        .bind(resource.id())
        .execute(&self.pool)
        .await
        .map_err(error)?
        .rows_affected();
        Ok(released == 1)
    }
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
        let root = &anchor.audience_root;
        let row = sqlx::query("SELECT owner_principal_id, visibility, revision, updated_by, updated_at_ms FROM access_resource_policies WHERE universe_id=$1 AND resource_kind=$2 AND resource_id=$3")
            .bind(universe).bind(root.kind()).bind(root.id()).fetch_optional(&self.pool).await.map_err(error)?;
        let Some(policy) = row.as_ref().map(policy_row).transpose()?.flatten() else {
            return Ok(None);
        };
        let grants = load_grants(&self.pool, universe, root).await?;
        Ok(Some(ResourcePolicyRecord {
            anchor,
            policy,
            grants,
        }))
    }

    /// Replace a root's visibility and grant set, and hand it to a new owner
    /// when the replacement names one, in one transaction. The policy row is
    /// locked first; the actor's rights and best grant are read after the
    /// lock, each statement on its own snapshot, so a right revoked while
    /// this waited, or at any time before, cannot commit. `expected_revision`,
    /// when supplied, guards only against editing a policy the caller has
    /// not seen. Every subject and a new owner must currently hold a role in
    /// the universe, directly or through a group, and a universe's execution
    /// principal only ever takes `use`; unchanged grants keep their
    /// attribution. The change is recorded and advances the deployment
    /// policy revision, so parked readers whose grant is gone revalidate and
    /// stop.
    pub async fn put_policy(
        &self,
        universe: Uuid,
        actor: Uuid,
        root: &ResourceRef,
        replacement: &PolicyReplacement,
        expected_revision: Option<u64>,
        now_ms: u64,
    ) -> Result<ResourcePolicyRecord, AccessError> {
        let now = i64::try_from(now_ms).map_err(error)?;
        let mut tx = self.pool.begin().await.map_err(error)?;
        let row = sqlx::query(
            "SELECT owner_principal_id, visibility, revision, updated_by, updated_at_ms
             FROM access_resource_policies WHERE universe_id=$1 AND resource_kind=$2 AND resource_id=$3 FOR UPDATE",
        )
        .bind(universe)
        .bind(root.kind())
        .bind(root.id())
        .fetch_optional(&mut *tx)
        .await
        .map_err(error)?
        .ok_or(AccessError::NotFound)?;
        let policy = policy_row(&row)?.ok_or(AccessError::NotFound)?;
        let rights = crate::access::effective(
            &mut tx,
            actor,
            AccessScope::Universe {
                universe_id: universe,
            },
        )
        .await?;
        let grant_rank = grant_rank_sql("p", "resource_kind", "resource_id", 4);
        let grant: Option<i32> = sqlx::query_scalar(&format!(
            "SELECT {grant_rank} FROM access_resource_policies p WHERE p.universe_id=$1 AND p.resource_kind=$2 AND p.resource_id=$3"
        ))
        .bind(universe)
        .bind(root.kind())
        .bind(root.id())
        .bind(actor)
        .fetch_one(&mut *tx)
        .await
        .map_err(error)?;
        let anchor = load_anchor(&mut *tx, universe, root)
            .await?
            .ok_or(AccessError::NotFound)?;
        let current_grants = load_grants(&mut *tx, universe, root).await?;
        let current = ResourceAccess {
            anchor,
            policy: Some(policy.clone()),
            grant: permission_from_rank(grant),
        };
        authorize_policy_replacement(&rights, &current, &current_grants, replacement)?;
        if let Some(expected) = expected_revision
            && expected != policy.revision
        {
            return Err(AccessError::RevisionMismatch {
                expected,
                actual: policy.revision,
            });
        }
        let owner = replacement.owner.filter(|owner| *owner != policy.owner);
        // A personal root runs as its owner; handing it over would run
        // someone's work under another person's authority.
        if owner.is_some()
            && current
                .anchor
                .execution
                .is_some_and(|execution| execution.kind == ExecutionKind::Personal)
        {
            return Err(AccessError::Invalid(
                "a root under personal execution is not handed off".into(),
            ));
        }
        // An agent identity uses resources; it never owns, reads or
        // controls a root.
        let agent_candidates: Vec<Uuid> = replacement
            .grants
            .iter()
            .filter(|(_, permission)| *permission != ResourcePermission::Use)
            .filter_map(|(subject, _)| match subject {
                Subject::Principal(id) => Some(*id),
                Subject::Group(_) => None,
            })
            .chain(owner)
            .collect();
        if !agent_candidates.is_empty() {
            let agent: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM universes WHERE execution_principal_id = ANY($1))",
            )
            .bind(&agent_candidates)
            .fetch_one(&mut *tx)
            .await
            .map_err(error)?;
            if agent {
                return Err(AccessError::Invalid(
                    "the Default agent identity only takes use grants and never owns a resource"
                        .into(),
                ));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        let subjects = replacement
            .grants
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
        let visibility = replacement.visibility;
        sqlx::query("UPDATE access_resource_policies SET visibility=$4, owner_principal_id=COALESCE($7, owner_principal_id), revision=revision+1, updated_by=$5, updated_at_ms=$6 WHERE universe_id=$1 AND resource_kind=$2 AND resource_id=$3")
            .bind(universe).bind(root.kind()).bind(root.id()).bind(visibility_name(visibility))
            .bind(json!(ActionActor::Principal { id: actor })).bind(now).bind(owner)
            .execute(&mut *tx).await.map_err(error)?;
        let keep: Vec<String> = replacement
            .grants
            .iter()
            .map(|(subject, _)| {
                let (kind, id) = subject_key(subject);
                format!("{kind}:{id}")
            })
            .collect();
        sqlx::query("DELETE FROM access_resource_grants WHERE universe_id=$1 AND resource_kind=$2 AND resource_id=$3 AND NOT (subject_kind || ':' || subject_id::text = ANY($4))")
            .bind(universe).bind(root.kind()).bind(root.id()).bind(&keep)
            .execute(&mut *tx).await.map_err(error)?;
        for (subject, permission) in &replacement.grants {
            let (subject_kind, subject_id) = subject_key(subject);
            sqlx::query("INSERT INTO access_resource_grants(universe_id,resource_kind,resource_id,subject_kind,subject_id,permission,granted_by,granted_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8)
                ON CONFLICT (universe_id,resource_kind,resource_id,subject_kind,subject_id) DO UPDATE
                SET permission=EXCLUDED.permission, granted_by=EXCLUDED.granted_by, granted_at_ms=EXCLUDED.granted_at_ms
                WHERE access_resource_grants.permission <> EXCLUDED.permission")
                .bind(universe).bind(root.kind()).bind(root.id()).bind(subject_kind).bind(subject_id)
                .bind(permission.as_str()).bind(actor).bind(now)
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
                "grants": replacement.grants.iter().map(|(subject, permission)| json!({"subject": subject, "permission": permission})).collect::<Vec<_>>(),
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
    /// Sees everything: internal maintenance, and an Admin listing
    /// workspaces, environments or MCP servers, whose restriction never hides
    /// them from Admin.
    Everything,
    Principal(Uuid),
    /// A person holding `read_private_content`: lists every resource and
    /// reports whether the page relied on the capability, so the read can
    /// be audited as privileged.
    Privileged(Uuid),
    Root(ResourceRef),
}

impl Reader {
    pub fn from_caller(caller: Caller<'_>) -> Self {
        match caller {
            Caller::Request(rights) if rights.has_capability(Capability::ReadPrivateContent) => {
                Reader::Privileged(rights.principal.id)
            }
            Caller::Request(rights) => Reader::Principal(rights.principal.id),
            Caller::Controller(context) => Reader::Root(context.root.clone()),
        }
    }

    /// The three bind values the predicate expects, in order.
    pub(crate) fn binds(&self) -> (Option<Uuid>, Option<&'static str>, Option<&str>) {
        match self {
            Reader::Everything => (None, None, None),
            Reader::Principal(id) | Reader::Privileged(id) => (Some(*id), None, None),
            Reader::Root(root) => (None, Some(root.kind()), Some(root.id())),
        }
    }
}

/// The SQL a list query needs for one reader: the row filter, and a column
/// expression that is true when the row is listed only because the reader
/// holds `read_private_content`.
pub(crate) struct ReadableClauses {
    pub filter: String,
    pub privileged: String,
}

/// Clauses over a content row: `alias.id_column` names a resource of
/// `kind`; `$p` is the reader's principal, `$p+1`/`$p+2` its root. A row
/// without an anchored, policied root is never listed.
pub(crate) fn readable_clauses(
    reader: &Reader,
    kind: &str,
    alias: &str,
    id_column: &str,
    p: usize,
) -> ReadableClauses {
    match reader {
        Reader::Everything => ReadableClauses {
            filter: "TRUE".to_owned(),
            privileged: "FALSE".to_owned(),
        },
        Reader::Privileged(_) => ReadableClauses {
            filter: "TRUE".to_owned(),
            privileged: format!("NOT {}", readable_predicate(kind, alias, id_column, p)),
        },
        Reader::Principal(_) | Reader::Root(_) => ReadableClauses {
            filter: readable_predicate(kind, alias, id_column, p),
            privileged: "FALSE".to_owned(),
        },
    }
}

/// The row filter of a list of workspaces, environments or MCP servers for
/// one reader, over the same bind layout as `readable_clauses`. Privileged
/// reading does not reach them, and internal work lists them as its
/// execution principal, never as a root.
pub(crate) fn operational_filter(
    reader: &Reader,
    kind: &str,
    alias: &str,
    id_column: &str,
    p: usize,
) -> String {
    match reader {
        Reader::Everything => "TRUE".to_owned(),
        Reader::Principal(_) | Reader::Privileged(_) => {
            readable_predicate(kind, alias, id_column, p)
        }
        Reader::Root(_) => "FALSE".to_owned(),
    }
}

/// Whether a list row was listed only through the privileged-read capability.
pub(crate) fn privileged_from_list_row(row: &sqlx::postgres::PgRow) -> Result<bool, AccessError> {
    row.try_get("access_privileged").map_err(error)
}

fn readable_predicate(kind: &str, alias: &str, id_column: &str, p: usize) -> String {
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

impl PgAccessStore {
    /// The universe's execution policy. The execution principal is created
    /// on first use: a keyless service principal managed in the universe,
    /// shown as the default agent identity, holding only Executor, and
    /// recorded on the universe row, which is what identifies it.
    pub async fn universe_execution_policy(
        &self,
        universe: Uuid,
        now_ms: u64,
    ) -> Result<UniverseExecutionPolicy, AccessError> {
        if let Some(policy) = self.read_execution_policy(universe).await? {
            return Ok(policy);
        }
        let now = i64::try_from(now_ms).map_err(error)?;
        let mut tx = self.pool.begin().await.map_err(error)?;
        let row = sqlx::query(
            "SELECT execution_principal_id FROM universes WHERE universe_id=$1 FOR UPDATE",
        )
        .bind(universe)
        .fetch_optional(&mut *tx)
        .await
        .map_err(error)?
        .ok_or(AccessError::NotFound)?;
        if row
            .try_get::<Option<Uuid>, _>("execution_principal_id")
            .map_err(error)?
            .is_none()
        {
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO access_principals(principal_id, kind, status, display_name, management_universe_id, created_at_ms) VALUES($1,'service','active','Default agent identity',$2,$3)")
                .bind(id).bind(universe).bind(now).execute(&mut *tx).await.map_err(error)?;
            sqlx::query("INSERT INTO access_role_assignments(universe_id, principal_id, role) VALUES($1,$2,'executor')")
                .bind(universe).bind(id).execute(&mut *tx).await.map_err(error)?;
            sqlx::query("UPDATE universes SET execution_principal_id=$2 WHERE universe_id=$1")
                .bind(universe)
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(error)?;
            crate::access::audit(
                &mut tx,
                None,
                json!({"operation": "create_execution_service", "universeId": universe, "principalId": id}),
                now,
            )
            .await?;
        }
        tx.commit().await.map_err(error)?;
        self.read_execution_policy(universe)
            .await?
            .ok_or(AccessError::NotFound)
    }

    async fn read_execution_policy(
        &self,
        universe: Uuid,
    ) -> Result<Option<UniverseExecutionPolicy>, AccessError> {
        let row = sqlx::query("SELECT execution_principal_id, personal_execution_enabled FROM universes WHERE universe_id=$1")
            .bind(universe).fetch_optional(&self.pool).await.map_err(error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let Some(execution_principal_id) = row
            .try_get::<Option<Uuid>, _>("execution_principal_id")
            .map_err(error)?
        else {
            return Ok(None);
        };
        Ok(Some(UniverseExecutionPolicy {
            execution_principal_id,
            personal_execution_enabled: row.try_get("personal_execution_enabled").map_err(error)?,
        }))
    }

    /// Recorded as an access change so the policy revision advances.
    pub async fn set_personal_execution_enabled(
        &self,
        universe: Uuid,
        actor: Uuid,
        enabled: bool,
        now_ms: u64,
    ) -> Result<UniverseExecutionPolicy, AccessError> {
        self.universe_execution_policy(universe, now_ms).await?;
        let now = i64::try_from(now_ms).map_err(error)?;
        let mut tx = self.pool.begin().await.map_err(error)?;
        sqlx::query("UPDATE universes SET personal_execution_enabled=$2 WHERE universe_id=$1")
            .bind(universe)
            .bind(enabled)
            .execute(&mut *tx)
            .await
            .map_err(error)?;
        crate::access::audit(
            &mut tx,
            Some(actor),
            json!({"operation": "set_personal_execution", "universeId": universe, "enabled": enabled}),
            now,
        )
        .await?;
        tx.commit().await.map_err(error)?;
        self.read_execution_policy(universe)
            .await?
            .ok_or(AccessError::NotFound)
    }
}

/// Content authorization: a blob is read through a resource the caller may
/// read, and only when it is admitted content of that resource. Admission
/// writes these rows; nothing here follows edges.
impl PgAccessStore {
    pub async fn record_blob_uploads(
        &self,
        universe: Uuid,
        principal: Uuid,
        digests: &[String],
        now_ms: u64,
    ) -> Result<(), AccessError> {
        let now = i64::try_from(now_ms).map_err(error)?;
        sqlx::query("INSERT INTO blob_uploads(universe_id, digest, principal_id, uploaded_at_ms) SELECT $1, digest, $2, $3 FROM unnest($4::text[]) AS u(digest) ON CONFLICT DO NOTHING")
            .bind(universe).bind(principal).bind(now).bind(digests)
            .execute(&self.pool).await.map_err(error)?;
        Ok(())
    }

    pub async fn uploaded_by(
        &self,
        universe: Uuid,
        principal: Uuid,
        digest: &str,
    ) -> Result<bool, AccessError> {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM blob_uploads WHERE universe_id=$1 AND principal_id=$2 AND digest=$3)")
            .bind(universe).bind(principal).bind(digest)
            .fetch_one(&self.pool).await.map_err(error)
    }

    /// Whether the blob is admitted content of the resource: a session's
    /// content roots or a bot's event roots. Nothing is admitted by digest
    /// for any other kind.
    pub async fn admitted_content(
        &self,
        universe: Uuid,
        resource: &ResourceRef,
        digest: &str,
    ) -> Result<bool, AccessError> {
        let sql = match resource {
            ResourceRef::Session(_) => {
                "SELECT EXISTS (SELECT 1 FROM cas_session_roots WHERE universe_id=$1 AND session_id=$2 AND digest=$3 AND origin='content')"
            }
            ResourceRef::Bot(_) => {
                "SELECT EXISTS (SELECT 1 FROM cas_bot_event_roots WHERE universe_id=$1 AND bot_id=$2 AND digest=$3 AND origin='content')"
            }
            ResourceRef::Profile(_)
            | ResourceRef::Workspace(_)
            | ResourceRef::Environment(_)
            | ResourceRef::McpServer(_) => return Ok(false),
        };
        sqlx::query_scalar(sql)
            .bind(universe)
            .bind(resource.id())
            .bind(digest)
            .fetch_one(&self.pool)
            .await
            .map_err(error)
    }

    /// Whether the blob is admitted content anywhere under a root: what
    /// internal work for that root may attach and read.
    pub async fn content_under_root(
        &self,
        universe: Uuid,
        root: &ResourceRef,
        digest: &str,
    ) -> Result<bool, AccessError> {
        sqlx::query_scalar("SELECT EXISTS (
                SELECT 1 FROM cas_session_roots r JOIN access_resources a
                  ON a.universe_id=r.universe_id AND a.resource_kind='session' AND a.resource_id=r.session_id
                WHERE r.universe_id=$1 AND r.digest=$4 AND r.origin='content' AND a.audience_root_kind=$2 AND a.audience_root_id=$3
                UNION ALL
                SELECT 1 FROM cas_bot_event_roots r JOIN access_resources a
                  ON a.universe_id=r.universe_id AND a.resource_kind='bot' AND a.resource_id=r.bot_id
                WHERE r.universe_id=$1 AND r.digest=$4 AND r.origin='content' AND a.audience_root_kind=$2 AND a.audience_root_id=$3)")
            .bind(universe).bind(root.kind()).bind(root.id()).bind(digest)
            .fetch_one(&self.pool).await.map_err(error)
    }
}
