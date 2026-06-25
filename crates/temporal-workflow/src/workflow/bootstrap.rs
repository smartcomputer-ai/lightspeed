use super::*;

pub(super) async fn initialize(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    args: AgentSessionArgs,
) -> anyhow::Result<()> {
    if ctx.workflow_id() != args.session_id.as_str() {
        anyhow::bail!(
            "agent workflow id must equal session id: workflow_id={} session_id={}",
            ctx.workflow_id(),
            args.session_id
        );
    }
    if ctx.state(|state| state.initialized) {
        return Ok(());
    }
    let observed_at_ms = workflow_time_ms(ctx);
    // the activity reduces the durable log internally and returns compact
    // state. The full event log no longer crosses the activity boundary, so this
    // bootstrap path is bounded by active context size, not total log length.
    let loaded = ctx
        .start_activity(
            WorkflowActivities::create_or_load_session,
            CreateOrLoadSessionRequest {
                session_id: args.session_id.clone(),
                observed_at_ms,
            },
            activity_options(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;

    let is_fresh_session = loaded.replayed_event_count == 0;
    let core_state = loaded.core_state.unwrap_or_else(CoreAgentState::new);
    let run_submissions = loaded.run_submissions;
    let head = loaded.head;
    ctx.state_mut(|state| {
        state.session_id = Some(args.session_id.clone());
        state.core_state = core_state;
        state.head = head;
        state.run_submissions = run_submissions;
        state.initialized = true;
        state.last_error = None;
    });

    if is_fresh_session {
        open_new_session(ctx, args).await?;
    }
    Ok(())
}

async fn open_new_session(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    args: AgentSessionArgs,
) -> anyhow::Result<()> {
    let instructions_ref = match args.instructions_ref.clone() {
        Some(blob_ref) => Some(blob_ref),
        None => {
            let blob_ref = ctx
                .start_activity(
                    WorkflowActivities::put_blob,
                    PutBlobRequest {
                        bytes: default_instructions().as_bytes().to_vec(),
                    },
                    activity_options(),
                )
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            Some(blob_ref)
        }
    };
    let session_config = args.session_config;

    let mut drive = drive_from_state(ctx)?;
    append_command(
        ctx,
        &mut drive,
        CoreAgentCommand::OpenSession {
            config: session_config,
        },
    )
    .await?;
    if let Some(instructions_ref) = instructions_ref {
        append_command(
            ctx,
            &mut drive,
            CoreAgentCommand::UpsertContext {
                key: ContextEntryKey::new("instructions.000.default"),
                entry: instruction_context_input(instructions_ref),
            },
        )
        .await?;
    }
    Ok(())
}

fn instruction_context_input(content_ref: BlobRef) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::Instructions,
        content_ref,
        media_type: Some("text/plain".to_owned()),
        preview: None,
        provider_kind: None,
        provider_item_id: None,
        token_estimate: None,
    }
}
