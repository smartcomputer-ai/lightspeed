//! Access records core keeps: who started a session or created a bot, and
//! which sessions are shared with the universe. Core holds no identities: actors are opaque strings a key asserted.
//!
//! A session's root is its `origin_root_session_id`, or the session itself;
//! a bot's session follows its bot, which is shared. Lineage is written only
//! by the runtime, so decisions may read it.

use api::{Attribution, ResourceAccessSummary, ResourceRef, Visibility};
use serde_json::json;
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("access store failed: {0}")]
pub struct AccessStoreError(String);

/// What internal work's decision about a target reads: who controls it and
/// the audience of its root, from the target's own row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceAccess {
    pub resource: ResourceRef,
    /// The bot whose worker controls it, if any.
    pub bot: Option<String>,
    /// The session that admitted it as a delegated child, if any.
    pub parent: Option<String>,
    /// The root whose audience it follows: itself, its root session, or its
    /// bot.
    pub root: ResourceRef,
    pub audience: ResourceAccessSummary,
}

impl ResourceAccess {
    /// A universe resource, or a bot: its own root, shared.
    pub fn shared(resource: ResourceRef, created_by: Option<Attribution>) -> Self {
        Self {
            bot: None,
            parent: None,
            root: resource.clone(),
            resource,
            audience: ResourceAccessSummary {
                visibility: Visibility::Universe,
                created_by,
            },
        }
    }
}

#[derive(Clone)]
pub struct PgAccessStore {
    pub(crate) pool: PgPool,
}

fn error(error: impl std::fmt::Display) -> AccessStoreError {
    AccessStoreError(error.to_string())
}

/// Joins the root of session `s` as `r`. A left join: a root that is gone
/// leaves its tree unshared and created by no one.
pub(crate) const SESSION_ROOT_JOIN: &str = "LEFT JOIN sessions r ON r.universe_id = s.universe_id AND r.session_id = COALESCE(s.origin_root_session_id, s.session_id)";

/// The bot whose worker controls session `s`'s tree, if any.
const SESSION_BOT: &str = "COALESCE(r.bot_id, s.bot_id)";

/// The visibility of session `s`'s root: a bot's tree is shared; a root
/// without a recorded visibility reads as unshared.
pub(crate) const SESSION_VISIBILITY: &str = "(CASE WHEN COALESCE(r.bot_id, s.bot_id) IS NOT NULL THEN 'universe' ELSE COALESCE(r.visibility, 'restricted') END)";

/// The summary columns a session list selects, aliased so they never collide
/// with the session's own.
pub(crate) fn session_summary_columns() -> String {
    format!("{SESSION_VISIBILITY} AS access_visibility, r.created_by AS access_created_by")
}

pub(crate) fn summary_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<ResourceAccessSummary, AccessStoreError> {
    let visibility: String = row.try_get("access_visibility").map_err(error)?;
    Ok(ResourceAccessSummary {
        visibility: Visibility::parse(&visibility)
            .ok_or_else(|| error(format!("unknown visibility {visibility}")))?,
        created_by: created_by_from_row(row, "access_created_by")?,
    })
}

pub(crate) fn created_by_from_row(
    row: &sqlx::postgres::PgRow,
    column: &str,
) -> Result<Option<Attribution>, AccessStoreError> {
    row.try_get::<Option<serde_json::Value>, _>(column)
        .map_err(error)?
        .map(serde_json::from_value)
        .transpose()
        .map_err(error)
}

/// `column` holds the attribution of the actor bound at `$bind`.
pub(crate) fn actor_match(column: &str, bind: usize) -> String {
    format!("{column} = jsonb_build_object('kind', 'actor', 'id', ${bind}::text)")
}

/// What a list of sessions is narrowed to, by the audience of each
/// session's root. Every present filter applies. Core applies what it is
/// asked; deciding who may see what belongs to whoever asserts actors.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessFilter {
    /// Only work whose root this actor created.
    pub created_by: Option<String>,
    /// Only work whose root has this visibility.
    pub visibility: Option<Visibility>,
    /// Only work shared with the universe or whose root this actor created.
    pub visible_to: Option<String>,
    /// Internal work: only work shared with the universe or under this root.
    pub within_root: Option<ResourceRef>,
}

impl AccessFilter {
    /// The predicates over session `s` and its root `r`, binding `$p` through
    /// `$p+4`, and a binder for the values in order.
    pub(crate) fn session_clause(&self, p: usize) -> (String, AccessFilterBinds) {
        let clause = format!(
            "AND (${p}::text IS NULL OR {created_by})
             AND (${visibility}::text IS NULL OR {SESSION_VISIBILITY} = ${visibility})
             AND (${visible}::text IS NULL OR {SESSION_VISIBILITY} = 'universe' OR {visible_to})
             AND (${root_kind}::text IS NULL OR {SESSION_VISIBILITY} = 'universe'
                  OR (${root_kind} = 'session' AND {SESSION_BOT} IS NULL
                      AND COALESCE(s.origin_root_session_id, s.session_id) = ${root_id})
                  OR (${root_kind} = 'bot' AND {SESSION_BOT} = ${root_id}))",
            created_by = actor_match("r.created_by", p),
            visibility = p + 1,
            visible = p + 2,
            visible_to = actor_match("r.created_by", p + 2),
            root_kind = p + 3,
            root_id = p + 4,
        );
        (
            clause,
            AccessFilterBinds {
                created_by: self.created_by.clone(),
                visibility: self.visibility.map(Visibility::as_str),
                visible_to: self.visible_to.clone(),
                root_kind: self.within_root.as_ref().map(ResourceRef::kind),
                root_id: self.within_root.as_ref().map(|root| root.id().to_owned()),
            },
        )
    }
}

/// The five values `AccessFilter::session_clause` binds, in order.
pub(crate) struct AccessFilterBinds {
    created_by: Option<String>,
    visibility: Option<&'static str>,
    visible_to: Option<String>,
    root_kind: Option<&'static str>,
    root_id: Option<String>,
}

impl AccessFilterBinds {
    pub(crate) fn bind<'q>(
        self,
        query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    ) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
        query
            .bind(self.created_by)
            .bind(self.visibility)
            .bind(self.visible_to)
            .bind(self.root_kind)
            .bind(self.root_id)
    }
}

/// The content row a resource names.
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

impl PgAccessStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Whether the resource exists in the universe.
    pub async fn resource_exists(
        &self,
        universe: Uuid,
        resource: &ResourceRef,
    ) -> Result<bool, AccessStoreError> {
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

    /// Record who started a session, once: a retry, or a stamp racing it,
    /// keeps the first. `visibility` is set on a request's root; `bot` on a
    /// bot's session. A delegated child is never stamped.
    pub async fn stamp_session(
        &self,
        universe: Uuid,
        session: &str,
        created_by: &Attribution,
        visibility: Option<Visibility>,
        bot: Option<&str>,
    ) -> Result<(), AccessStoreError> {
        sqlx::query(
            "UPDATE sessions SET created_by = $3, visibility = $4, bot_id = $5
             WHERE universe_id = $1 AND session_id = $2 AND created_by IS NULL",
        )
        .bind(universe)
        .bind(session)
        .bind(json!(created_by))
        .bind(visibility.map(Visibility::as_str))
        .bind(bot)
        .execute(&self.pool)
        .await
        .map_err(error)?;
        Ok(())
    }

    /// Record who created a bot, once.
    pub async fn stamp_bot(
        &self,
        universe: Uuid,
        bot: &str,
        created_by: &Attribution,
    ) -> Result<(), AccessStoreError> {
        sqlx::query(
            "UPDATE bots SET created_by = $3 WHERE universe_id = $1 AND bot_id = $2 AND created_by IS NULL",
        )
        .bind(universe)
        .bind(bot)
        .bind(json!(created_by))
        .execute(&self.pool)
        .await
        .map_err(error)?;
        Ok(())
    }

    /// Who controls a session and the audience of its root, from its row;
    /// `None` when it does not exist.
    pub async fn session_access(
        &self,
        universe: Uuid,
        session: &str,
    ) -> Result<Option<ResourceAccess>, AccessStoreError> {
        let summary = session_summary_columns();
        let row = sqlx::query(&format!(
            "SELECT s.origin_parent_session_id, COALESCE(s.origin_root_session_id, s.session_id) AS root_session_id,
                    {SESSION_BOT} AS root_bot_id, {summary}
             FROM sessions s {SESSION_ROOT_JOIN}
             WHERE s.universe_id = $1 AND s.session_id = $2"
        ))
        .bind(universe)
        .bind(session)
        .fetch_optional(&self.pool)
        .await
        .map_err(error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let bot: Option<String> = row.try_get("root_bot_id").map_err(error)?;
        Ok(Some(ResourceAccess {
            resource: ResourceRef::Session(session.to_owned()),
            root: match &bot {
                Some(bot) => ResourceRef::Bot(bot.clone()),
                None => ResourceRef::Session(row.try_get("root_session_id").map_err(error)?),
            },
            bot,
            parent: row.try_get("origin_parent_session_id").map_err(error)?,
            audience: summary_from_row(&row)?,
        }))
    }

    /// A bot's access facts; `None` when it does not exist.
    pub async fn bot_access(
        &self,
        universe: Uuid,
        bot: &str,
    ) -> Result<Option<ResourceAccess>, AccessStoreError> {
        let row = sqlx::query("SELECT created_by FROM bots WHERE universe_id = $1 AND bot_id = $2")
            .bind(universe)
            .bind(bot)
            .fetch_optional(&self.pool)
            .await
            .map_err(error)?;
        row.map(|row| {
            Ok(ResourceAccess::shared(
                ResourceRef::Bot(bot.to_owned()),
                created_by_from_row(&row, "created_by")?,
            ))
        })
        .transpose()
    }

    /// Share an unshared root session with the universe. Returns whether it
    /// changed; `false` when it is already shared, a bot's session, or a
    /// delegated child.
    pub async fn share_session(
        &self,
        universe: Uuid,
        session: &str,
    ) -> Result<bool, AccessStoreError> {
        Ok(sqlx::query(
            "UPDATE sessions SET visibility = 'universe'
             WHERE universe_id = $1 AND session_id = $2 AND origin_root_session_id IS NULL
               AND bot_id IS NULL AND COALESCE(visibility, 'restricted') = 'restricted'",
        )
        .bind(universe)
        .bind(session)
        .execute(&self.pool)
        .await
        .map_err(error)?
        .rows_affected()
            == 1)
    }

    /// The retention tree determines cascade deletion targets. It is broader
    /// than controller lineage: a history fork is its own root.
    pub async fn session_deletion_targets(
        &self,
        universe: Uuid,
        session: &str,
    ) -> Result<Vec<String>, AccessStoreError> {
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
}
