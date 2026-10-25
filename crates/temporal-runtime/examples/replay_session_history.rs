//! Offline replay of explicitly selected, retained session workflow histories.
use temporal_workflow::AgentSessionWorkflow;
use temporalio_client::WorkflowHistory;
use temporalio_sdk::workflow_replayer::{WorkflowReplayer, WorkflowReplayerOptions};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let mut paths = std::env::args().skip(1).collect::<Vec<_>>();
    if paths.first().map(String::as_str) == Some("--capture-local") {
        let client =
            temporal_workflow::connect_temporal("http://127.0.0.1:17233", "default").await?;
        let mut captured = Vec::new();
        for index in &paths[1..] {
            let entries: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(index)?)?;
            for entry in entries {
                let id = entry["workflowId"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing workflowId"))?;
                let bytes = client
                    .get_workflow_handle::<AgentSessionWorkflow>(id)
                    .fetch_history(Default::default())
                    .to_json()
                    .await?;
                let path = std::path::Path::new(index)
                    .parent()
                    .unwrap()
                    .join(format!("replay-session-{}.json", entry["sessionIndex"]));
                anyhow::ensure!(!path.exists(), "refusing to overwrite {}", path.display());
                std::fs::write(&path, bytes)?;
                captured.push(path.to_string_lossy().into_owned());
            }
        }
        paths = captured;
    }
    anyhow::ensure!(
        !paths.is_empty(),
        "supply one or more Temporal history JSON paths"
    );
    let replayer = WorkflowReplayer::new(
        WorkflowReplayerOptions::new()
            .register_workflow::<AgentSessionWorkflow>()?
            .build(),
    )?;
    for path in paths {
        let history = WorkflowHistory::from_json(&std::fs::read(&path)?)?;
        anyhow::ensure!(
            history.workflow_id().is_some(),
            "history lacks workflow ID: {path}"
        );
        replayer.replay_workflow(history).await?;
        println!("replay passed: {path}");
    }
    Ok(())
}
