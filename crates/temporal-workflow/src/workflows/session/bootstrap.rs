use super::*;

pub(super) async fn initialize(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    args: AgentSessionArgs,
) -> anyhow::Result<()> {
    let expected_workflow_id = compose_workflow_id(args.universe_id, &args.session_id);
    if ctx.workflow_id() != expected_workflow_id {
        anyhow::bail!(
            "agent workflow id must equal {{universe_id}}/{{session_id}}: workflow_id={} expected={}",
            ctx.workflow_id(),
            expected_workflow_id
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
                display_name: args.display_name.clone(),
                observed_at_ms,
            },
            activity_options(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;

    let is_fresh_session = loaded.replayed_event_count == 0;
    let core_state = loaded.core_state.unwrap_or_else(CoreAgentState::new);
    validate_session_creation_identity(
        args.universe_id,
        &core_state,
        is_fresh_session,
        args.controller_ports.as_ref(),
    )?;
    let run_submissions = loaded.run_submissions;
    let head = loaded.head;
    ctx.state_mut(|state| {
        state.universe_id = Some(args.universe_id);
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
    let instructions_ref = ctx
        .start_activity(
            WorkflowActivities::put_blob,
            PutBlobRequest {
                bytes: default_instructions().as_bytes().to_vec(),
            },
            activity_options(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let session_config = args.session_config;

    let mut drive = drive_from_state(ctx)?;
    let open_command = match args.controller_ports {
        Some(controller_ports) => CoreAgentCommand::OpenManagedSession {
            config: session_config,
            session_universe_id: args.universe_id,
            controller_ports,
        },
        None => CoreAgentCommand::OpenSession {
            config: session_config,
        },
    };
    append_command(ctx, &mut drive, open_command).await?;
    append_command(
        ctx,
        &mut drive,
        CoreAgentCommand::UpsertContext {
            expected_revision: None,
            key: ContextEntryKey::new("instructions.000.default"),
            entry: instruction_context_input(instructions_ref),
        },
    )
    .await?;
    Ok(())
}

pub(super) fn validate_session_creation_identity(
    universe_id: uuid::Uuid,
    state: &CoreAgentState,
    is_fresh_session: bool,
    controller_ports: Option<&engine::ControllerWorkflowPorts>,
) -> anyhow::Result<()> {
    let admitted = controller_ports
        .map(|ports| ports.admit(universe_id))
        .transpose()?;
    if is_fresh_session {
        return Ok(());
    }

    match (
        state.workflow_ports.managed_creation_fingerprint.as_deref(),
        admitted.as_ref(),
    ) {
        (None, None) => Ok(()),
        (Some(actual), Some(expected))
            if state.workflow_ports.session_universe_id == Some(universe_id)
                && actual == expected.creation_fingerprint =>
        {
            Ok(())
        }
        (Some(_), None) => {
            anyhow::bail!("managed session restart is missing its controller declaration")
        }
        (None, Some(_)) => {
            anyhow::bail!("existing standalone session cannot be reopened as a managed session")
        }
        (Some(_), Some(_)) => {
            anyhow::bail!("managed session creation fingerprint conflicts with durable state")
        }
    }
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
