//! Recent interactive sessions are selected by the runtime in the current universe.
use anyhow::{Context, Result};

use crate::api_client::{HttpAgentApi, api_error};

async fn recent(api: &HttpAgentApi, resume: bool) -> Result<Vec<api::SessionSummaryView>> {
    Ok(api
        .list_sessions(api::SessionListParams {
            limit: Some(if resume { 1 } else { 10 }),
            managed: Some(false),
            subagent: Some(false),
            closed: resume.then_some(false),
            ..Default::default()
        })
        .await
        .map_err(api_error)?
        .result
        .sessions)
}

pub(super) async fn resume_id(api: &HttpAgentApi) -> Result<String> {
    recent(api, true)
        .await?
        .into_iter()
        .next()
        .map(|session| session.id)
        .context("No resumable unmanaged chat sessions in this universe. Start one with `lightspeed chat`.")
}

pub(super) async fn list(api: &HttpAgentApi, json: bool) -> Result<()> {
    let sessions = recent(api, false).await?;
    if json {
        return crate::output::show(true, &sessions);
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let rows: Vec<_> = sessions
        .iter()
        .map(|session| {
            serde_json::json!({
                "id": session.id,
                "name": session.display_name,
                "status": session.lifecycle_status,
                "activity": session.activity,
                "updated": updated_ago(now_ms.saturating_sub(session.updated_at_ms)),
            })
        })
        .collect();
    crate::output::table(
        &rows,
        &[
            ("id", "SESSION"),
            ("name", "NAME"),
            ("status", "STATUS"),
            ("activity", "ACTIVITY"),
            ("updated", "UPDATED"),
        ],
        "No unmanaged chat sessions in this universe. Start one with `lightspeed chat`.",
    )?;
    if !sessions.is_empty() {
        println!(
            "\nContinue the latest open chat: lightspeed chat --resume\nOpen a specific session: lightspeed chat -s SESSION_ID"
        );
    }
    Ok(())
}

fn updated_ago(elapsed_ms: u64) -> String {
    let seconds = elapsed_ms / 1_000;
    match seconds {
        0..60 => "just now".into(),
        60..3_600 => format!("{}m ago", seconds / 60),
        3_600..86_400 => format!("{}h ago", seconds / 3_600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}
