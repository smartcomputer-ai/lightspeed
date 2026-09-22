//! Canonical identity store. Access writes serialize on one deployment policy
//! row; readers use a committed snapshot and never an authorization cache.

use ::access::*;
use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool, Row, postgres::PgRow};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgAccessStore {
    pub(crate) pool: PgPool,
}

impl PgAccessStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn scope_id(scope: AccessScope) -> Option<Uuid> {
    match scope {
        AccessScope::Deployment => None,
        AccessScope::Universe { universe_id } => Some(universe_id),
    }
}
fn scope_from_id(id: Option<Uuid>) -> AccessScope {
    id.map_or(AccessScope::Deployment, |universe_id| {
        AccessScope::Universe { universe_id }
    })
}
fn enum_name<T: Serialize>(value: T) -> Result<String, AccessError> {
    serde_json::to_value(value)
        .map_err(store_error)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| AccessError::Store("invalid stored enum".into()))
}
pub(crate) fn parse_enum<T: DeserializeOwned>(value: String) -> Result<T, AccessError> {
    serde_json::from_value(Value::String(value)).map_err(store_error)
}
fn store_error(error: impl std::fmt::Display) -> AccessError {
    AccessError::Store(error.to_string())
}
fn db_error(error: sqlx::Error) -> AccessError {
    if let sqlx::Error::Database(ref db) = error {
        if db.is_unique_violation() {
            return AccessError::Conflict;
        }
        if db.is_foreign_key_violation() {
            return AccessError::NotFound;
        }
    }
    store_error(error)
}
fn timestamp(value: u64) -> Result<i64, AccessError> {
    i64::try_from(value).map_err(|_| AccessError::Invalid("timestamp exceeds storage range".into()))
}
fn nonnegative(value: i64) -> Result<u64, AccessError> {
    u64::try_from(value).map_err(store_error)
}

fn principal_row(row: PgRow) -> Result<Principal, AccessError> {
    Ok(Principal {
        id: row.try_get("principal_id").map_err(db_error)?,
        kind: parse_enum(row.try_get("kind").map_err(db_error)?)?,
        status: parse_enum(row.try_get("status").map_err(db_error)?)?,
        display_name: row.try_get("display_name").map_err(db_error)?,
        management_scope: scope_from_id(row.try_get("management_universe_id").map_err(db_error)?),
        created_at_ms: nonnegative(row.try_get("created_at_ms").map_err(db_error)?)?,
    })
}
async fn read_principal(connection: &mut PgConnection, id: Uuid) -> Result<Principal, AccessError> {
    sqlx::query("SELECT * FROM access_principals WHERE principal_id = $1")
        .bind(id)
        .fetch_optional(connection)
        .await
        .map_err(db_error)?
        .ok_or(AccessError::NotFound)
        .and_then(principal_row)
}
async fn revision(connection: &mut PgConnection) -> Result<u64, AccessError> {
    nonnegative(
        sqlx::query_scalar("SELECT revision FROM access_policy WHERE singleton")
            .fetch_one(connection)
            .await
            .map_err(db_error)?,
    )
}
/// One statement, hence one committed snapshot: the principal, its direct and
/// group-derived roles, its capabilities in `scope`, and the policy revision.
/// Disabled principals resolve with no rights.
pub(crate) async fn effective(
    connection: &mut PgConnection,
    id: Uuid,
    scope: AccessScope,
) -> Result<EffectiveAccess, AccessError> {
    let row = sqlx::query(
        "SELECT p.*,
           (SELECT revision FROM access_policy WHERE singleton) AS policy_revision,
           ARRAY(SELECT DISTINCT r.role FROM access_role_assignments r
             WHERE p.status = 'active' AND r.universe_id IS NOT DISTINCT FROM $2 AND
               (r.principal_id = p.principal_id OR EXISTS
                 (SELECT 1 FROM access_memberships m
                   WHERE m.group_id = r.group_id AND m.principal_id = p.principal_id))) AS roles,
           ARRAY(SELECT c.capability FROM access_capabilities c
             WHERE p.status = 'active' AND p.kind = 'service' AND
               c.universe_id IS NOT DISTINCT FROM $2 AND c.principal_id = p.principal_id) AS capabilities
         FROM access_principals p WHERE p.principal_id = $1",
    )
    .bind(id)
    .bind(scope_id(scope))
    .fetch_optional(connection)
    .await
    .map_err(db_error)?
    .ok_or(AccessError::NotFound)?;
    let roles: Vec<String> = row.try_get("roles").map_err(db_error)?;
    let capabilities: Vec<String> = row.try_get("capabilities").map_err(db_error)?;
    Ok(EffectiveAccess {
        scope,
        roles: roles
            .into_iter()
            .map(parse_enum)
            .collect::<Result<_, _>>()?,
        capabilities: capabilities
            .into_iter()
            .map(parse_enum)
            .collect::<Result<_, _>>()?,
        policy_revision: nonnegative(row.try_get("policy_revision").map_err(db_error)?)?,
        principal: principal_row(row)?,
    })
}

/// Every scope which currently has at least one active administrator. Comparing
/// before/after handles direct and group roles, duplicate assignments, and group
/// removals spanning several universes without blocking unrelated orphan scopes.
async fn administered_scopes(
    connection: &mut PgConnection,
) -> Result<Vec<AccessScope>, AccessError> {
    let rows: Vec<Option<Uuid>> = sqlx::query_scalar(
        "SELECT DISTINCT r.universe_id FROM access_role_assignments r
         WHERE r.role IN ('admin', 'deployment_admin') AND EXISTS
           (SELECT 1 FROM access_principals p WHERE p.status = 'active' AND
             (r.principal_id = p.principal_id OR EXISTS
               (SELECT 1 FROM access_memberships m WHERE m.group_id = r.group_id AND m.principal_id = p.principal_id)))")
        .fetch_all(connection).await.map_err(db_error)?;
    Ok(rows.into_iter().map(scope_from_id).collect())
}
fn guard_administrators(before: &[AccessScope], after: &[AccessScope]) -> Result<(), AccessError> {
    for scope in before {
        if !after.contains(scope) {
            return Err(AccessError::LastAdministrator { scope: *scope });
        }
    }
    Ok(())
}
pub(crate) async fn audit(
    connection: &mut PgConnection,
    actor: Option<Uuid>,
    event: Value,
    now_ms: i64,
) -> Result<u64, AccessError> {
    let next: i64 = sqlx::query_scalar(
        "UPDATE access_policy SET revision = revision + 1 WHERE singleton RETURNING revision",
    )
    .fetch_one(&mut *connection)
    .await
    .map_err(db_error)?;
    sqlx::query("INSERT INTO access_audit_changes (revision, actor_id, occurred_at_ms, event) VALUES ($1, $2, $3, $4)")
        .bind(next).bind(actor).bind(now_ms).bind(event).execute(connection).await.map_err(db_error)?;
    nonnegative(next)
}

async fn authorize_change(
    connection: &mut PgConnection,
    actor: Uuid,
    change: &AccessChange,
) -> Result<(), AccessError> {
    let deployment = effective(connection, actor, AccessScope::Deployment).await?;
    if !deployment.active() {
        return Err(AccessError::Denied);
    }
    if deployment.has_role(Role::DeploymentAdmin) {
        return Ok(());
    }
    // Provisioning delegates directory changes, including privileged group
    // membership. Direct role/capability assignment and recovery are separate.
    let provisioning = deployment.has_capability(ServiceCapability::ManageIdentity);
    match change {
        AccessChange::AssignRole { assignment }
        | AccessChange::RevokeRole { assignment }
        | AccessChange::ReplaceRole { assignment, .. }
            if matches!(assignment.scope, AccessScope::Universe { .. }) =>
        {
            if effective(connection, actor, assignment.scope)
                .await?
                .has_role(Role::Admin)
            {
                return Ok(());
            }
        }
        AccessChange::CreatePrincipal {
            kind: PrincipalKind::Service,
            management_scope: scope @ AccessScope::Universe { .. },
            ..
        } => {
            if effective(connection, actor, *scope)
                .await?
                .has_role(Role::Admin)
            {
                return Ok(());
            }
        }
        AccessChange::CreatePrincipal {
            kind: PrincipalKind::User,
            ..
        }
        | AccessChange::CreateGroup { .. }
        | AccessChange::RenameGroup { .. }
        | AccessChange::PutMembership { .. }
        | AccessChange::RemoveMembership { .. }
        | AccessChange::SetPrincipalStatus { .. }
            if provisioning =>
        {
            // Group edits/status changes can affect privileged identities. A
            // provisioning service therefore has explicitly delegated directory
            // authority, including offboarding; never grant this by service kind.
            return Ok(());
        }
        _ => {}
    }
    Err(AccessError::Denied)
}

async fn put_role(
    connection: &mut PgConnection,
    assignment: RoleAssignment,
) -> Result<bool, AccessError> {
    let (principal, group) = match assignment.subject {
        Subject::Principal(id) => (Some(id), None),
        Subject::Group(id) => (None, Some(id)),
    };
    Ok(sqlx::query("INSERT INTO access_role_assignments (universe_id, principal_id, group_id, role) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING")
        .bind(scope_id(assignment.scope)).bind(principal).bind(group).bind(enum_name(assignment.role)?)
        .execute(connection).await.map_err(db_error)?.rows_affected() > 0)
}

async fn apply_change(
    connection: &mut PgConnection,
    actor: Uuid,
    change: &AccessChange,
    now_ms: i64,
) -> Result<bool, AccessError> {
    use AccessChange::*;
    let changed = match change {
        CreatePrincipal { id, kind, display_name, management_scope } => {
            if let AccessScope::Universe { universe_id } = management_scope {
                let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM universes WHERE universe_id = $1)")
                    .bind(universe_id).fetch_one(&mut *connection).await.map_err(db_error)?;
                if !exists { return Err(AccessError::NotFound); }
            }
            let inserted = sqlx::query("INSERT INTO access_principals (principal_id, kind, status, display_name, management_universe_id, created_at_ms) VALUES ($1, $2, 'active', $3, $4, $5) ON CONFLICT DO NOTHING")
                .bind(id).bind(enum_name(kind)?).bind(display_name).bind(scope_id(*management_scope)).bind(now_ms)
                .execute(&mut *connection).await.map_err(db_error)?.rows_affected();
            if inserted == 0 {
                let existing = read_principal(connection, *id).await?;
                if existing.kind != *kind || existing.management_scope != *management_scope || existing.display_name != *display_name {
                    return Err(AccessError::Conflict);
                }
                // A retry never reactivates a disabled principal.
            }
            inserted
        }
        SetPrincipalStatus { id, status } => {
            let prior = read_principal(connection, *id).await?;
            if prior.status == *status { return Ok(false); }
            sqlx::query("UPDATE access_principals SET status = $2 WHERE principal_id = $1")
                .bind(id).bind(enum_name(status)?).execute(connection).await.map_err(db_error)?.rows_affected()
        }
        CreateGroup { id, display_name } => {
            sqlx::query("INSERT INTO access_groups (group_id, display_name, created_at_ms) VALUES ($1, $2, $3)")
                .bind(id).bind(display_name).bind(now_ms).execute(connection).await.map_err(db_error)?.rows_affected()
        }
        RenameGroup { id, display_name } => {
            let prior: Option<String> = sqlx::query_scalar("SELECT display_name FROM access_groups WHERE group_id = $1")
                .bind(id).fetch_optional(&mut *connection).await.map_err(db_error)?;
            match prior { None => return Err(AccessError::NotFound), Some(ref name) if name == display_name => return Ok(false), _ => {} }
            sqlx::query("UPDATE access_groups SET display_name = $2 WHERE group_id = $1")
                .bind(id).bind(display_name).execute(connection).await.map_err(db_error)?.rows_affected()
        }
        PutMembership { membership } => {
            sqlx::query("INSERT INTO access_memberships (group_id, principal_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
                .bind(membership.group_id).bind(membership.principal_id).execute(connection).await.map_err(db_error)?.rows_affected()
        }
        RemoveMembership { membership } => {
            sqlx::query("DELETE FROM access_memberships WHERE group_id = $1 AND principal_id = $2")
                .bind(membership.group_id).bind(membership.principal_id).execute(connection).await.map_err(db_error)?.rows_affected()
        }
        AssignRole { assignment } => return put_role(connection, *assignment).await,
        RevokeRole { assignment } => {
            let (principal, group) = match assignment.subject { Subject::Principal(id) => (Some(id), None), Subject::Group(id) => (None, Some(id)) };
            sqlx::query("DELETE FROM access_role_assignments WHERE universe_id IS NOT DISTINCT FROM $1 AND principal_id IS NOT DISTINCT FROM $2 AND group_id IS NOT DISTINCT FROM $3 AND role = $4")
                .bind(scope_id(assignment.scope)).bind(principal).bind(group).bind(enum_name(assignment.role)?)
                .execute(connection).await.map_err(db_error)?.rows_affected()
        }
        ReplaceRole { assignment, role } => {
            let (principal, group) = match assignment.subject { Subject::Principal(id) => (Some(id), None), Subject::Group(id) => (None, Some(id)) };
            let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM access_role_assignments WHERE universe_id IS NOT DISTINCT FROM $1 AND principal_id IS NOT DISTINCT FROM $2 AND group_id IS NOT DISTINCT FROM $3 AND role = $4)")
                .bind(scope_id(assignment.scope)).bind(principal).bind(group).bind(enum_name(assignment.role)?)
                .fetch_one(&mut *connection).await.map_err(db_error)?;
            if !exists { return Err(AccessError::Conflict); }
            if *role == assignment.role { return Ok(false); }
            sqlx::query("DELETE FROM access_role_assignments WHERE universe_id IS NOT DISTINCT FROM $1 AND principal_id IS NOT DISTINCT FROM $2 AND group_id IS NOT DISTINCT FROM $3 AND role = $4")
                .bind(scope_id(assignment.scope)).bind(principal).bind(group).bind(enum_name(assignment.role)?)
                .execute(&mut *connection).await.map_err(db_error)?;
            put_role(connection, RoleAssignment { role: *role, ..*assignment }).await?;
            1
        }
        AssignCapability { assignment } | RevokeCapability { assignment } => {
            let principal = read_principal(connection, assignment.principal_id).await?;
            if principal.kind != PrincipalKind::Service { return Err(AccessError::Invalid("capabilities require a service principal".into())); }
            // Universe-managed services must never acquire deployment-wide powers
            // or capabilities in a different universe.
            if !principal.management_scope.permits(assignment.scope) {
                return Err(AccessError::Invalid("capability exceeds service management scope".into()));
            }
            let query = if matches!(change, AssignCapability { .. }) {
                "INSERT INTO access_capabilities (universe_id, principal_id, capability) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING"
            } else { "DELETE FROM access_capabilities WHERE universe_id IS NOT DISTINCT FROM $1 AND principal_id = $2 AND capability = $3" };
            sqlx::query(query).bind(scope_id(assignment.scope)).bind(assignment.principal_id).bind(enum_name(assignment.capability)?)
                .execute(connection).await.map_err(db_error)?.rows_affected()
        }
        CreateUniverse { universe_id, slug } => {
            let inserted = sqlx::query("INSERT INTO universes (universe_id, slug) VALUES ($1, $2) ON CONFLICT (universe_id) DO NOTHING")
                .bind(universe_id).bind(slug).execute(&mut *connection).await.map_err(db_error)?.rows_affected();
            // An idempotent create cannot take over an existing universe.
            if inserted == 0 { return Ok(false); }
            put_role(connection, RoleAssignment { scope: AccessScope::Universe { universe_id: *universe_id }, subject: Subject::Principal(actor), role: Role::Admin }).await?;
            inserted
        }
        RecoverUniverse { universe_id, principal_id } => {
            let scope = AccessScope::Universe { universe_id: *universe_id };
            if administered_scopes(connection).await?.contains(&scope) { return Err(AccessError::Conflict); }
            if read_principal(connection, *principal_id).await?.status != PrincipalStatus::Active { return Err(AccessError::Invalid("recovery requires an active principal".into())); }
            return put_role(connection, RoleAssignment { scope, subject: Subject::Principal(*principal_id), role: Role::Admin }).await;
        }
    };
    Ok(changed > 0)
}

#[async_trait]
impl AccessStore for PgAccessStore {
    async fn principal(&self, id: Uuid) -> Result<Option<Principal>, AccessError> {
        sqlx::query("SELECT * FROM access_principals WHERE principal_id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_error)?
            .map(principal_row)
            .transpose()
    }

    async fn effective_access(
        &self,
        principal_id: Uuid,
        scope: AccessScope,
    ) -> Result<EffectiveAccess, AccessError> {
        let mut connection = self.pool.acquire().await.map_err(db_error)?;
        effective(&mut connection, principal_id, scope).await
    }

    async fn accessible_universes(&self, principal_id: Uuid) -> Result<Vec<Uuid>, AccessError> {
        // A single statement sees one committed snapshot; no deployment-admin
        // bypass and no response containing other principals' memberships.
        sqlx::query_scalar(
            "SELECT DISTINCT r.universe_id FROM access_role_assignments r
             JOIN access_principals p ON p.principal_id = $1 AND p.status = 'active'
             WHERE r.universe_id IS NOT NULL AND
               (r.principal_id = p.principal_id OR EXISTS
                 (SELECT 1 FROM access_memberships m WHERE m.group_id = r.group_id AND m.principal_id = p.principal_id))
             ORDER BY r.universe_id")
            .bind(principal_id).fetch_all(&self.pool).await.map_err(db_error)
    }

    async fn apply(
        &self,
        actor: Uuid,
        change: AccessChange,
        now_ms: u64,
    ) -> Result<AccessChangeResult, AccessError> {
        change.validate()?;
        let now_ms = timestamp(now_ms)?;
        let mut transaction = self.pool.begin().await.map_err(db_error)?;
        sqlx::query("SELECT revision FROM access_policy WHERE singleton FOR UPDATE")
            .execute(&mut *transaction)
            .await
            .map_err(db_error)?;
        authorize_change(&mut transaction, actor, &change).await?;
        let before = administered_scopes(&mut transaction).await?;
        let changed = apply_change(&mut transaction, actor, &change, now_ms).await?;
        // Disabling an identity is always possible, even when it or its groups
        // supply the last administrator. Recovery is a separate audited action.
        if changed
            && !matches!(
                change,
                AccessChange::SetPrincipalStatus {
                    status: PrincipalStatus::Disabled,
                    ..
                }
            )
        {
            guard_administrators(&before, &administered_scopes(&mut transaction).await?)?;
        }
        let policy_revision = if changed {
            audit(
                &mut transaction,
                Some(actor),
                serde_json::to_value(&change).map_err(store_error)?,
                now_ms,
            )
            .await?
        } else {
            revision(&mut transaction).await?
        };
        transaction.commit().await.map_err(db_error)?;
        Ok(AccessChangeResult {
            changed,
            policy_revision,
        })
    }

    async fn bootstrap(
        &self,
        principal_id: Uuid,
        display_name: String,
        now_ms: u64,
    ) -> Result<AccessChangeResult, AccessError> {
        let change = AccessChange::CreatePrincipal {
            id: principal_id,
            kind: PrincipalKind::User,
            display_name,
            management_scope: AccessScope::Deployment,
        };
        change.validate()?;
        let now_ms = timestamp(now_ms)?;
        let mut transaction = self.pool.begin().await.map_err(db_error)?;
        let bootstrapped: Option<Uuid> = sqlx::query_scalar(
            "SELECT bootstrap_principal_id FROM access_policy WHERE singleton FOR UPDATE",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(db_error)?;
        if let Some(existing) = bootstrapped {
            if existing != principal_id
                || !effective(&mut transaction, principal_id, AccessScope::Deployment)
                    .await?
                    .has_role(Role::DeploymentAdmin)
            {
                return Err(AccessError::AlreadyBootstrapped);
            }
            let policy_revision = revision(&mut transaction).await?;
            transaction.commit().await.map_err(db_error)?;
            return Ok(AccessChangeResult {
                changed: false,
                policy_revision,
            });
        }
        apply_change(&mut transaction, principal_id, &change, now_ms).await?;
        if read_principal(&mut transaction, principal_id).await?.status != PrincipalStatus::Active {
            return Err(AccessError::Denied);
        }
        put_role(
            &mut transaction,
            RoleAssignment {
                scope: AccessScope::Deployment,
                subject: Subject::Principal(principal_id),
                role: Role::DeploymentAdmin,
            },
        )
        .await?;
        sqlx::query("UPDATE access_policy SET bootstrap_principal_id = $1 WHERE singleton")
            .bind(principal_id)
            .execute(&mut *transaction)
            .await
            .map_err(db_error)?;
        let policy_revision = audit(
            &mut transaction,
            None,
            json!({ "operation": "bootstrap", "principalId": principal_id }),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(db_error)?;
        Ok(AccessChangeResult {
            changed: true,
            policy_revision,
        })
    }
}

impl PgAccessStore {
    /// The committed policy revision. Every identity, role, capability, key
    /// and universe-removal change advances it, so an unchanged revision means
    /// previously resolved rights still hold.
    pub async fn policy_revision(&self) -> Result<u64, AccessError> {
        let mut connection = self.pool.acquire().await.map_err(db_error)?;
        revision(&mut connection).await
    }

    /// Whether an authenticated service may act for a user in `target`: an
    /// `assert_user` capability in that scope, or deployment-wide when the
    /// presented credential itself is deployment-scoped.
    pub async fn may_assert_user(
        &self,
        service: Uuid,
        target: AccessScope,
        credential_scope: AccessScope,
    ) -> Result<bool, AccessError> {
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM access_capabilities c \
             JOIN access_principals p ON p.principal_id = c.principal_id \
             WHERE c.principal_id = $1 AND c.capability = 'assert_user' \
               AND p.status = 'active' AND p.kind = 'service' \
               AND (c.universe_id IS NOT DISTINCT FROM $2 OR ($3 AND c.universe_id IS NULL)))",
        )
        .bind(service)
        .bind(scope_id(target))
        .bind(credential_scope == AccessScope::Deployment)
        .fetch_one(&self.pool)
        .await
        .map_err(db_error)
    }
}

/// Stable, explicit identity used only by the opt-in local development gateway.
pub const LOCAL_DEVELOPMENT_PRINCIPAL: Uuid =
    Uuid::from_u128(0x6c696768_7473_4065_8064_000000000001);

impl PgAccessStore {
    /// Host-side initialization for `single` mode. Calling this is an explicit
    /// development bootstrap, never an authenticated request fallback.
    pub async fn initialize_local_development(
        &self,
        universe_id: Uuid,
        now_ms: u64,
    ) -> Result<Principal, AccessError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        sqlx::query("SELECT revision FROM access_policy WHERE singleton FOR UPDATE")
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let id = LOCAL_DEVELOPMENT_PRINCIPAL;
        let mut changed = apply_change(
            &mut tx,
            id,
            &AccessChange::CreatePrincipal {
                id,
                kind: PrincipalKind::Service,
                display_name: "Local development".into(),
                management_scope: AccessScope::Deployment,
            },
            timestamp(now_ms)?,
        )
        .await?;
        let principal = read_principal(&mut tx, id).await?;
        if principal.status != PrincipalStatus::Active {
            return Err(AccessError::Denied);
        }
        for (scope, role) in [
            (AccessScope::Deployment, Role::DeploymentAdmin),
            (AccessScope::Universe { universe_id }, Role::Admin),
        ] {
            changed |= put_role(
                &mut tx,
                RoleAssignment {
                    scope,
                    subject: Subject::Principal(id),
                    role,
                },
            )
            .await?;
        }
        for (scope, capability) in [
            (
                AccessScope::Deployment,
                ServiceCapability::DiscoverChannelAccounts,
            ),
            (
                AccessScope::Universe { universe_id },
                ServiceCapability::LeaseCredentials,
            ),
            (
                AccessScope::Universe { universe_id },
                ServiceCapability::AdmitChannelInbound,
            ),
        ] {
            changed |= apply_change(
                &mut tx,
                id,
                &AccessChange::AssignCapability {
                    assignment: CapabilityAssignment {
                        scope,
                        principal_id: id,
                        capability,
                    },
                },
                timestamp(now_ms)?,
            )
            .await?;
        }
        if changed {
            audit(&mut tx, None, json!({"operation":"local_development_initialized", "principalId":id, "universeId":universe_id}), timestamp(now_ms)?).await?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(principal)
    }
}

impl PgAccessStore {
    /// Administrative view from one committed snapshot. In deployment scope
    /// the directory is complete; in universe scope it holds the subjects of
    /// that universe: principals and groups holding a role there, members of
    /// those groups, and service principals it manages. A universe
    /// administrator learns nothing about the rest of the deployment.
    pub async fn directory(
        &self,
        actor: Uuid,
        scope: AccessScope,
    ) -> Result<AccessDirectory, AccessError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let deployment = effective(&mut tx, actor, AccessScope::Deployment).await?;
        let global = deployment.has_role(Role::DeploymentAdmin)
            || deployment.has_capability(ServiceCapability::ManageIdentity);
        if !global
            && !effective(&mut tx, actor, scope)
                .await?
                .has_role(Role::Admin)
        {
            return Err(AccessError::Denied);
        }
        let all = scope == AccessScope::Deployment;
        let principals = sqlx::query(
            "SELECT p.* FROM access_principals p WHERE $1
             OR p.management_universe_id=$2
             OR EXISTS (SELECT 1 FROM access_role_assignments r WHERE r.universe_id=$2 AND r.principal_id=p.principal_id)
             OR EXISTS (SELECT 1 FROM access_memberships m JOIN access_role_assignments r ON r.group_id=m.group_id
                        WHERE r.universe_id=$2 AND m.principal_id=p.principal_id)
             ORDER BY p.principal_id",
        )
        .bind(all)
        .bind(scope_id(scope))
        .fetch_all(&mut *tx)
        .await
        .map_err(db_error)?
        .into_iter()
        .map(principal_row)
        .collect::<Result<Vec<_>, _>>()?;
        let groups = sqlx::query(
            "SELECT g.* FROM access_groups g WHERE $1
             OR EXISTS (SELECT 1 FROM access_role_assignments r WHERE r.universe_id=$2 AND r.group_id=g.group_id)
             ORDER BY g.group_id",
        )
        .bind(all)
        .bind(scope_id(scope))
        .fetch_all(&mut *tx)
        .await
        .map_err(db_error)?
        .into_iter()
            .map(|r| {
                Ok(Group {
                    id: r.try_get("group_id").map_err(db_error)?,
                    display_name: r.try_get("display_name").map_err(db_error)?,
                    created_at_ms: nonnegative(r.try_get("created_at_ms").map_err(db_error)?)?,
                })
            })
            .collect::<Result<Vec<_>, AccessError>>()?;
        let memberships = sqlx::query("SELECT m.* FROM access_memberships m WHERE $1 OR EXISTS (SELECT 1 FROM access_role_assignments r WHERE r.group_id=m.group_id AND r.universe_id=$2) ORDER BY group_id, principal_id")
            .bind(all).bind(scope_id(scope)).fetch_all(&mut *tx).await.map_err(db_error)?.into_iter().map(|r| Ok(Membership {
                group_id: r.try_get("group_id").map_err(db_error)?, principal_id: r.try_get("principal_id").map_err(db_error)?,
            })).collect::<Result<Vec<_>, AccessError>>()?;
        let roles = sqlx::query("SELECT * FROM access_role_assignments WHERE $1 OR universe_id=$2 ORDER BY universe_id, principal_id, group_id, role")
            .bind(all).bind(scope_id(scope)).fetch_all(&mut *tx).await.map_err(db_error)?.into_iter().map(|r| {
                let principal: Option<Uuid> = r.try_get("principal_id").map_err(db_error)?;
                Ok(RoleAssignment { scope: scope_from_id(r.try_get("universe_id").map_err(db_error)?),
                    subject: match principal { Some(id) => Subject::Principal(id), None => Subject::Group(r.try_get("group_id").map_err(db_error)?) },
                    role: parse_enum(r.try_get("role").map_err(db_error)?)?,
                })
            }).collect::<Result<Vec<_>, AccessError>>()?;
        let capabilities = sqlx::query("SELECT * FROM access_capabilities WHERE $1 OR universe_id=$2 ORDER BY universe_id, principal_id, capability")
            .bind(all).bind(scope_id(scope)).fetch_all(&mut *tx).await.map_err(db_error)?.into_iter().map(|r| Ok(CapabilityAssignment {
                scope: scope_from_id(r.try_get("universe_id").map_err(db_error)?),
                principal_id: r.try_get("principal_id").map_err(db_error)?, capability: parse_enum(r.try_get("capability").map_err(db_error)?)?,
            })).collect::<Result<Vec<_>, AccessError>>()?;
        let policy_revision = revision(&mut tx).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(AccessDirectory {
            principals,
            groups,
            memberships,
            roles,
            capabilities,
            policy_revision,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_admin_guard_handles_preexisting_orphans_and_multiple_scopes() {
        let a = AccessScope::Universe {
            universe_id: Uuid::from_u128(1),
        };
        let b = AccessScope::Universe {
            universe_id: Uuid::from_u128(2),
        };
        assert_eq!(
            guard_administrators(&[a, b], &[a]),
            Err(AccessError::LastAdministrator { scope: b })
        );
        assert!(guard_administrators(&[a], &[a, b]).is_ok());
        assert!(guard_administrators(&[], &[]).is_ok());
        assert_eq!(
            guard_administrators(&[AccessScope::Deployment], &[]),
            Err(AccessError::LastAdministrator {
                scope: AccessScope::Deployment
            })
        );
    }
}
