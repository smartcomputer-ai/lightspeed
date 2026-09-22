//! `lightspeed session …`: start, list, tag, close, and delete sessions.
//!
//! Metadata is the grouping primitive. A repeatable `--metadata key=value`
//! stamps a session at start, filters `list`, and selects what `close` and
//! `delete` act on. The API has no bulk operation by design: the filtered
//! list is the primitive and this command loops over it.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use clap::{Args, Subcommand};

use crate::api_client::{HttpAgentApi, api_error};
use crate::env_cli::print_json_or;

#[derive(Args, Debug, Clone)]
pub(crate) struct SessionArgs {
    #[command(subcommand)]
    command: SessionCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum SessionCommand {
    /// Start a session, optionally from a named profile and with metadata.
    Start(StartArgs),
    /// List sessions, newest activity first, optionally filtered by metadata.
    List(ListArgs),
    /// Replace a session's metadata map.
    Metadata(MetadataCommandArgs),
    /// Set or clear automatic deletion for a retention root.
    Retention(RetentionArgs),
    /// Close one session by id, or every open session matching a filter.
    Close(CloseArgs),
    /// Delete one closed session by id, or every closed session matching a
    /// filter; open matches are skipped.
    Delete(DeleteArgs),
}

#[derive(Args, Debug, Clone)]
struct CommonArgs {
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    /// Print the API response as JSON.
    #[arg(long)]
    json: bool,
}

/// Repeatable `--metadata key=value`; the pairs form one map.
#[derive(Args, Debug, Clone, Default)]
pub(crate) struct MetadataPairs {
    /// A metadata pair; repeat for more. Bounded like environment metadata:
    /// keys up to 64 bytes, values up to 256, no `lightspeed.` prefix.
    #[arg(long = "metadata", value_name = "KEY=VALUE", value_parser = parse_metadata_pair)]
    pairs: Vec<(String, String)>,
}

impl MetadataPairs {
    pub(crate) fn map(&self) -> BTreeMap<String, String> {
        self.pairs.iter().cloned().collect()
    }
}

fn parse_metadata_pair(raw: &str) -> Result<(String, String), String> {
    match raw.split_once('=') {
        Some((key, value)) if !key.is_empty() && !value.is_empty() => {
            Ok((key.to_owned(), value.to_owned()))
        }
        _ => Err(format!("expected KEY=VALUE, got {raw:?}")),
    }
}

#[derive(Args, Debug, Clone)]
struct StartArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Client-chosen session id; the server mints one when omitted.
    #[arg(long = "session-id")]
    session_id: Option<String>,
    #[arg(long = "display-name")]
    display_name: Option<String>,
    /// Named profile from the universe catalog.
    #[arg(long)]
    profile: Option<String>,
    #[command(flatten)]
    metadata: MetadataPairs,
    /// Delete this retention tree after the root closes (for example 30m,
    /// 24h, or 7d).
    #[arg(long = "delete-after", value_parser = parse_duration_ms)]
    delete_after_close_ms: Option<u64>,
}

#[derive(Args, Debug, Clone)]
struct ListArgs {
    #[command(flatten)]
    common: CommonArgs,
    #[command(flatten)]
    metadata: MetadataPairs,
    /// Only sub-agent sessions whose lineage root is this session.
    #[arg(long = "root")]
    root_session_id: Option<String>,
    /// Only sub-agent sessions delegated directly by this session.
    #[arg(long = "parent")]
    parent_session_id: Option<String>,
    /// Page size; pages are followed until the list is exhausted.
    #[arg(long, default_value_t = 100)]
    limit: u32,
}

#[derive(Args, Debug, Clone)]
struct MetadataCommandArgs {
    #[command(subcommand)]
    command: MetadataCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum MetadataCommand {
    /// Replace the whole map; with no --metadata the map is cleared.
    Put(MetadataPutArgs),
}

#[derive(Args, Debug, Clone)]
struct MetadataPutArgs {
    #[command(flatten)]
    common: CommonArgs,
    session_id: String,
    #[command(flatten)]
    metadata: MetadataPairs,
}

#[derive(Args, Debug, Clone)]
struct RetentionArgs {
    #[command(flatten)]
    common: CommonArgs,
    session_id: String,
    /// Delete the tree this long after its root closes (for example 24h).
    #[arg(long = "delete-after", value_parser = parse_duration_ms, conflicts_with = "off")]
    delete_after_close_ms: Option<u64>,
    /// Disable automatic deletion.
    #[arg(long, required_unless_present = "delete_after_close_ms")]
    off: bool,
}

#[derive(Args, Debug, Clone)]
struct CloseArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Session to close; omit it to close every open session matching
    /// --metadata.
    session_id: Option<String>,
    #[command(flatten)]
    metadata: MetadataPairs,
    /// Cancel active and queued work instead of refusing on it.
    #[arg(long)]
    force: bool,
}

#[derive(Args, Debug, Clone)]
struct DeleteArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Session to delete; omit it to delete every closed session matching
    /// --metadata.
    session_id: Option<String>,
    #[command(flatten)]
    metadata: MetadataPairs,
    /// Also delete history forks and delegated child sessions.
    #[arg(long)]
    cascade: bool,
}

pub(crate) async fn handle(args: SessionArgs) -> Result<()> {
    match args.command {
        SessionCommand::Start(args) => start(args).await,
        SessionCommand::List(args) => list(args).await,
        SessionCommand::Metadata(args) => match args.command {
            MetadataCommand::Put(args) => put_metadata(args).await,
        },
        SessionCommand::Retention(args) => put_retention(args).await,
        SessionCommand::Close(args) => close(args).await,
        SessionCommand::Delete(args) => delete(args).await,
    }
}

async fn start(args: StartArgs) -> Result<()> {
    let profile = args
        .profile
        .map(|profile_id| {
            api::ProfileId::try_new(profile_id)
                .map(|profile_id| api::ProfileSource::Named { profile_id })
        })
        .transpose()
        .map_err(|error| anyhow::anyhow!("invalid profile id: {error}"))?;
    let response = HttpAgentApi::new(args.common.api_url)
        .start_session(api::SessionStartParams {
            access: None,
            execution: None,
            session_id: args.session_id,
            display_name: args.display_name,
            metadata: args.metadata.map(),
            config: None,
            profile,
            delete_after_close_ms: args.delete_after_close_ms.map(Some),
        })
        .await
        .map_err(api_error)?
        .result;
    print_json_or(args.common.json, &response, || {
        println!("{}", response.session.id);
    })
}

async fn list(args: ListArgs) -> Result<()> {
    let client = HttpAgentApi::new(args.common.api_url);
    let sessions = collect_sessions(
        &client,
        Selection {
            metadata: args.metadata.map(),
            root_session_id: args.root_session_id,
            parent_session_id: args.parent_session_id,
            limit: args.limit,
        },
    )
    .await?;
    print_json_or(args.common.json, &sessions, || {
        for session in &sessions {
            println!("{}", session_line(session));
        }
    })
}

async fn put_metadata(args: MetadataPutArgs) -> Result<()> {
    let response = HttpAgentApi::new(args.common.api_url)
        .put_session_metadata(api::SessionMetadataPutParams {
            session_id: args.session_id,
            metadata: args.metadata.map(),
        })
        .await
        .map_err(api_error)?
        .result;
    print_json_or(args.common.json, &response, || {
        println!("{}", session_line(&response.session));
    })
}

async fn put_retention(args: RetentionArgs) -> Result<()> {
    let response = HttpAgentApi::new(args.common.api_url)
        .put_session_retention(api::SessionRetentionPutParams {
            session_id: args.session_id,
            delete_after_close_ms: if args.off {
                None
            } else {
                args.delete_after_close_ms
            },
        })
        .await
        .map_err(api_error)?
        .result;
    print_json_or(args.common.json, &response, || {
        println!("{}", session_line(&response.session));
    })
}

async fn close(args: CloseArgs) -> Result<()> {
    let client = HttpAgentApi::new(args.common.api_url);
    let targets = select_targets(
        &client,
        args.session_id,
        &args.metadata,
        "close",
        |session| session.lifecycle_status != api::SessionLifecycleStatus::Closed,
    )
    .await?;
    let mut done = Vec::new();
    let mut failed = 0;
    for session_id in targets.ids {
        match client
            .close_session(api::SessionCloseParams {
                session_id: session_id.clone(),
                force: args.force,
            })
            .await
        {
            Ok(_) => done.push(session_id),
            Err(error) => {
                failed += 1;
                eprintln!("close {session_id}: {}", api_error(error));
            }
        }
    }
    report(args.common.json, "closed", &done, targets.skipped, failed)
}

async fn delete(args: DeleteArgs) -> Result<()> {
    let client = HttpAgentApi::new(args.common.api_url);
    let targets = select_targets(
        &client,
        args.session_id,
        &args.metadata,
        "delete",
        |session| session.lifecycle_status == api::SessionLifecycleStatus::Closed,
    )
    .await?;
    let mut done = Vec::new();
    let mut failed = 0;
    for session_id in targets.ids {
        match client
            .delete_session(api::SessionDeleteParams {
                session_id: session_id.clone(),
                cascade: args.cascade,
            })
            .await
        {
            Ok(_) => done.push(session_id),
            Err(error) => {
                failed += 1;
                eprintln!("delete {session_id}: {}", api_error(error));
            }
        }
    }
    report(args.common.json, "deleted", &done, targets.skipped, failed)
}

fn parse_duration_ms(raw: &str) -> Result<u64, String> {
    let split = raw
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(raw.len());
    let (amount, unit) = raw.split_at(split);
    let amount = amount
        .parse::<u64>()
        .map_err(|_| format!("expected a positive duration, got {raw:?}"))?;
    if amount == 0 {
        return Err("duration must be positive".to_owned());
    }
    let multiplier = match unit {
        "ms" | "" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        "w" => 604_800_000,
        _ => {
            return Err(format!(
                "unsupported duration unit in {raw:?}; use ms, s, m, h, d, or w"
            ));
        }
    };
    amount
        .checked_mul(multiplier)
        .ok_or_else(|| format!("duration is too large: {raw:?}"))
}

struct Selection {
    metadata: BTreeMap<String, String>,
    root_session_id: Option<String>,
    parent_session_id: Option<String>,
    limit: u32,
}

/// Follow `session/list` cursors until the selection is exhausted.
async fn collect_sessions(
    client: &HttpAgentApi,
    selection: Selection,
) -> Result<Vec<api::SessionSummaryView>> {
    let mut sessions = Vec::new();
    let mut cursor = None;
    loop {
        let page = client
            .list_sessions(api::SessionListParams {
                cursor,
                limit: Some(selection.limit),
                root_session_id: selection.root_session_id.clone(),
                parent_session_id: selection.parent_session_id.clone(),
                exclude_closed: false,
                metadata: selection.metadata.clone(),
            })
            .await
            .map_err(api_error)?
            .result;
        sessions.extend(page.sessions);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return Ok(sessions),
        }
    }
}

struct Targets {
    ids: Vec<String>,
    /// Matches the filter but not the lifecycle the verb needs.
    skipped: usize,
}

/// One explicit id, or every filtered session in the lifecycle `keep`
/// accepts. An empty filter is refused: it would mean every session.
async fn select_targets(
    client: &HttpAgentApi,
    session_id: Option<String>,
    metadata: &MetadataPairs,
    verb: &str,
    keep: impl Fn(&api::SessionSummaryView) -> bool,
) -> Result<Targets> {
    if let Some(session_id) = session_id {
        if !metadata.pairs.is_empty() {
            bail!("pass either a session id or --metadata, not both");
        }
        return Ok(Targets {
            ids: vec![session_id],
            skipped: 0,
        });
    }
    let filter = metadata.map();
    if filter.is_empty() {
        bail!(
            "{verb} needs a session id or at least one --metadata pair; an empty filter would select every session"
        );
    }
    let sessions = collect_sessions(
        client,
        Selection {
            metadata: filter,
            root_session_id: None,
            parent_session_id: None,
            limit: 100,
        },
    )
    .await?;
    let total = sessions.len();
    let ids: Vec<String> = sessions
        .into_iter()
        .filter(|session| keep(session))
        .map(|session| session.id)
        .collect();
    Ok(Targets {
        skipped: total - ids.len(),
        ids,
    })
}

fn report(json: bool, verb: &str, done: &[String], skipped: usize, failed: usize) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                verb: done,
                "skipped": skipped,
                "failed": failed,
            }))?
        );
    } else {
        for session_id in done {
            println!("{verb} {session_id}");
        }
        if skipped > 0 {
            println!("skipped {skipped} (wrong lifecycle for {verb})");
        }
        if done.is_empty() && skipped == 0 && failed == 0 {
            println!("no matching sessions");
        }
    }
    if failed > 0 {
        bail!("{failed} session(s) could not be {verb}");
    }
    Ok(())
}

fn session_line(session: &api::SessionSummaryView) -> String {
    let status = match session.lifecycle_status {
        api::SessionLifecycleStatus::New => "new",
        api::SessionLifecycleStatus::Open => "open",
        api::SessionLifecycleStatus::Closed => "closed",
    };
    let metadata = session
        .metadata
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{} {} {} {}",
        session.id,
        status,
        session.display_name.as_deref().unwrap_or("-"),
        if metadata.is_empty() { "-" } else { &metadata }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_pairs_parse_and_collect() {
        assert_eq!(
            parse_metadata_pair("job=nightly"),
            Ok(("job".to_owned(), "nightly".to_owned()))
        );
        assert_eq!(
            parse_metadata_pair("url=https://x/?a=b"),
            Ok(("url".to_owned(), "https://x/?a=b".to_owned()))
        );
        assert!(parse_metadata_pair("novalue").is_err());
        assert!(parse_metadata_pair("=v").is_err());
        assert!(parse_metadata_pair("k=").is_err());
        let pairs = MetadataPairs {
            pairs: vec![
                ("b".to_owned(), "1".to_owned()),
                ("a".to_owned(), "2".to_owned()),
                ("b".to_owned(), "3".to_owned()),
            ],
        };
        assert_eq!(
            pairs.map(),
            BTreeMap::from([
                ("a".to_owned(), "2".to_owned()),
                ("b".to_owned(), "3".to_owned())
            ])
        );
    }

    #[test]
    fn session_line_shows_metadata_or_dash() {
        let mut session = api::SessionSummaryView {
            access: api::ResourceAccessSummary {
                root: api::ResourceRef::Session("session_1".to_owned()),
                owner: "00000000-0000-0000-0000-000000000000".parse().unwrap(),
                visibility: api::Visibility::Universe,
                execution: None,
            },
            id: "s1".to_owned(),
            display_name: None,
            metadata: BTreeMap::new(),
            lifecycle_status: api::SessionLifecycleStatus::Open,
            closed_at_ms: None,
            retention: api::SessionRetentionView {
                root_session_id: "s1".to_owned(),
                delete_after_close_ms: None,
                delete_at_ms: None,
            },
            managed: false,
            origin: None,
            created_at_ms: 0,
            updated_at_ms: 0,
        };
        assert_eq!(session_line(&session), "s1 open - -");
        session
            .metadata
            .insert("job".to_owned(), "nightly".to_owned());
        session.display_name = Some("Task".to_owned());
        session.lifecycle_status = api::SessionLifecycleStatus::Closed;
        assert_eq!(session_line(&session), "s1 closed Task job=nightly");
    }

    #[test]
    fn delete_after_duration_parser_accepts_human_units() {
        assert_eq!(parse_duration_ms("1500"), Ok(1_500));
        assert_eq!(parse_duration_ms("30m"), Ok(1_800_000));
        assert_eq!(parse_duration_ms("24h"), Ok(86_400_000));
        assert_eq!(parse_duration_ms("7d"), Ok(604_800_000));
        assert!(parse_duration_ms("0").is_err());
        assert!(parse_duration_ms("1month").is_err());
    }
}
