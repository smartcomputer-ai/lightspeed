use std::collections::{BTreeMap, BTreeSet};

use engine::{PromiseSourceCheckResult, storage::BlobStore};
use environments::{
    CreateJobHandle, EnvironmentId, EnvironmentInstanceId, EnvironmentInstanceStore,
    EnvironmentJobGroupId, EnvironmentJobGroupStatus, JobHandleStore,
    SessionEnvironmentBindingState, SessionEnvironmentBindingStore,
    UpdateEnvironmentJobGroupStatus,
};
use host_client::{HostDataClient, WebSocketConnectOptions};
use host_protocol::{
    data::{
        handshake::{InitializeParams, InitializedParams},
        jobs::{CancelJobsParams, JobStatus, ReadJobsParams},
    },
    shared::{CURRENT_PROTOCOL_VERSION, HostConnectionSpec, HostTransport},
};
use store_pg::PgStore;
use temporal_workflow::{
    EnvironmentJobCancelActivityRequest, EnvironmentJobPollActivityRequest,
    EnvironmentJobPollActivityResult, EnvironmentJobStartActivityRequest,
    EnvironmentJobStartActivityResult, EnvironmentJobStartPayload,
};
use temporalio_sdk::activities::ActivityError;

use super::common::activity_error;
use crate::credential_injection::EnvironmentCredentialResolver;

const PROMISE_JOB_OUTPUT_BYTES: usize = 16 * 1024;

pub(super) async fn start(
    store: Option<&std::sync::Arc<PgStore>>,
    request: EnvironmentJobStartActivityRequest,
) -> Result<EnvironmentJobStartActivityResult, ActivityError> {
    let store = store.ok_or_else(|| {
        activity_error(anyhow::anyhow!(
            "environment job activities are not configured"
        ))
    })?;
    let instance_id =
        EnvironmentInstanceId::try_new(request.instance_id.clone()).map_err(activity_error)?;
    let group_id =
        EnvironmentJobGroupId::try_new(request.job_group_id.clone()).map_err(activity_error)?;
    let mut payload: EnvironmentJobStartPayload = serde_json::from_slice(
        &store
            .read_bytes(&request.request_ref)
            .await
            .map_err(activity_error)?,
    )
    .map_err(activity_error)?;
    if payload.request.namespace != instance_id.as_str() {
        return Err(activity_error(anyhow::anyhow!(
            "environment job start namespace does not match instance {instance_id}"
        )));
    }

    if let Some(scope) = payload.credential_scope.take() {
        let env_id = EnvironmentId::try_new(scope.env_id).map_err(activity_error)?;
        let binding = store
            .read_binding(&scope.session_id, &env_id)
            .await
            .map_err(activity_error)?;
        if binding.state != SessionEnvironmentBindingState::Attached
            || binding.instance_id != instance_id
        {
            return Err(activity_error(anyhow::anyhow!(
                "environment job credential scope no longer refers to attached instance {instance_id}"
            )));
        }
        let resolver = EnvironmentCredentialResolver::from_pg_store(store.clone());
        for job in &mut payload.request.jobs {
            let secret_env = resolver
                .resolve_secret_env(&scope.session_id, &env_id, &job.env)
                .await
                .map_err(activity_error)?;
            job.secret_env.extend(secret_env);
        }
    }

    // Failed attempts stay Pending while Temporal retries. Marking the group
    // failed per attempt would make instance close race a still-live workflow.
    start_on_provider(store.as_ref(), &instance_id, &group_id, &payload).await
}

async fn start_on_provider(
    store: &PgStore,
    instance_id: &EnvironmentInstanceId,
    group_id: &EnvironmentJobGroupId,
    payload: &EnvironmentJobStartPayload,
) -> Result<EnvironmentJobStartActivityResult, ActivityError> {
    let group = store
        .read_job_group(instance_id, group_id)
        .await
        .map_err(activity_error)?;
    if group.start_request_hash != payload.start_request_hash
        || group.request_id != payload.request.request_id
    {
        return Err(activity_error(anyhow::anyhow!(
            "environment job workflow start does not match reserved group {instance_id}/{group_id}"
        )));
    }
    let instance = store
        .read_instance(instance_id)
        .await
        .map_err(activity_error)?;
    let (mut client, capabilities) = initialized_client(&instance.connection).await?;
    if !capabilities.job_start {
        return Err(activity_error(anyhow::anyhow!(
            "environment does not support durable job start: {instance_id}"
        )));
    }
    let response = client
        .start_jobs(&payload.request)
        .await
        .map_err(activity_error)?;
    let requested_ids = payload
        .request
        .jobs
        .iter()
        .map(|job| job.job_id.clone())
        .collect::<BTreeSet<_>>();
    let returned_ids = response
        .jobs
        .iter()
        .map(|job| job.job_id.clone())
        .collect::<BTreeSet<_>>();
    if requested_ids != returned_ids {
        return Err(activity_error(anyhow::anyhow!(
            "environment provider start response job ids do not match the request"
        )));
    }
    let created_at_ms = unix_ms()?;
    let records = response
        .jobs
        .iter()
        .map(|summary| CreateJobHandle {
            instance_id: instance_id.clone(),
            job_group_id: group_id.clone(),
            job_id: summary.job_id.clone(),
            name: summary.name.clone(),
            queue_key: summary.queue_key.clone(),
            created_by_session_id: payload.provenance.session_id.clone(),
            created_by_run_id: payload.provenance.run_id,
            created_by_turn_id: payload.provenance.turn_id,
            created_by_tool_call_id: payload.provenance.tool_call_id.clone(),
            created_at_ms,
            start_request_hash: payload.start_request_hash.clone(),
        })
        .collect();
    store
        .create_job_handles(records)
        .await
        .map_err(activity_error)?;
    store
        .update_job_group_status(UpdateEnvironmentJobGroupStatus {
            instance_id: instance_id.clone(),
            job_group_id: group_id.clone(),
            // The first workflow poll captures terminal payload/error refs and
            // is the only transition that marks the group terminal.
            status: EnvironmentJobGroupStatus::Running,
            updated_at_ms: created_at_ms,
        })
        .await
        .map_err(activity_error)?;
    Ok(EnvironmentJobStartActivityResult {
        jobs: response.jobs,
    })
}

pub(super) async fn poll(
    store: Option<&PgStore>,
    request: EnvironmentJobPollActivityRequest,
) -> Result<EnvironmentJobPollActivityResult, ActivityError> {
    let store = store.ok_or_else(|| {
        activity_error(anyhow::anyhow!(
            "environment job activities are not configured"
        ))
    })?;
    let instance_id =
        EnvironmentInstanceId::try_new(request.instance_id).map_err(activity_error)?;
    let group_id = EnvironmentJobGroupId::try_new(request.job_group_id).map_err(activity_error)?;
    let instance = store
        .read_instance(&instance_id)
        .await
        .map_err(activity_error)?;
    let (mut client, _) = initialized_client(&instance.connection).await?;
    let requested_job_ids = request.job_ids.iter().cloned().collect::<BTreeSet<_>>();
    let response = client
        .read_jobs(&ReadJobsParams {
            namespace: instance_id.as_str().to_owned(),
            jobs: request.job_ids,
            after_seq: None,
            max_bytes: Some(PROMISE_JOB_OUTPUT_BYTES),
            include_artifacts: false,
            wait_ms: None,
        })
        .await
        .map_err(activity_error)?;
    let mut jobs = Vec::with_capacity(response.jobs.len());
    let mut resolutions = BTreeMap::new();
    for result in response.jobs {
        let summary = result.summary.clone();
        if summary.status.is_terminal() {
            let resolution = if summary.status == JobStatus::Succeeded {
                let payload_ref = store
                    .put_bytes(serde_json::to_vec(&result).map_err(activity_error)?)
                    .await
                    .map_err(activity_error)?;
                PromiseSourceCheckResult::Resolved {
                    payload_ref: Some(payload_ref),
                }
            } else {
                let message = summary.failure.clone().unwrap_or_else(|| {
                    format!(
                        "environment job {} ended as {:?}",
                        summary.job_id, summary.status
                    )
                });
                let error_ref = store
                    .put_bytes(message.into_bytes())
                    .await
                    .map_err(activity_error)?;
                PromiseSourceCheckResult::Failed {
                    error_ref: Some(error_ref),
                }
            };
            resolutions.insert(summary.job_id.as_str().to_owned(), resolution);
        }
        jobs.push(summary);
    }
    let returned_job_ids = jobs
        .iter()
        .map(|job| job.job_id.clone())
        .collect::<BTreeSet<_>>();
    let terminal = returned_job_ids == requested_job_ids
        && !jobs.is_empty()
        && jobs.iter().all(|job| job.status.is_terminal());
    if terminal {
        store
            .update_job_group_status(UpdateEnvironmentJobGroupStatus {
                instance_id,
                job_group_id: group_id,
                status: EnvironmentJobGroupStatus::Terminal,
                updated_at_ms: unix_ms()?,
            })
            .await
            .map_err(activity_error)?;
    }
    Ok(EnvironmentJobPollActivityResult {
        jobs,
        resolutions,
        terminal,
    })
}

pub(super) async fn cancel(
    store: Option<&PgStore>,
    request: EnvironmentJobCancelActivityRequest,
) -> Result<Vec<host_protocol::data::jobs::JobSummary>, ActivityError> {
    let store = store.ok_or_else(|| {
        activity_error(anyhow::anyhow!(
            "environment job activities are not configured"
        ))
    })?;
    let instance_id =
        EnvironmentInstanceId::try_new(request.instance_id).map_err(activity_error)?;
    let instance = store
        .read_instance(&instance_id)
        .await
        .map_err(activity_error)?;
    let (mut client, _) = initialized_client(&instance.connection).await?;
    let response = client
        .cancel_jobs(&CancelJobsParams {
            namespace: instance_id.as_str().to_owned(),
            jobs: request.jobs,
            scope: request.scope,
            force: request.force,
        })
        .await
        .map_err(activity_error)?;
    Ok(response.jobs)
}

async fn initialized_client(
    connection: &HostConnectionSpec,
) -> Result<
    (
        HostDataClient<host_client::WebSocketTransport>,
        host_protocol::shared::HostCapabilities,
    ),
    ActivityError,
> {
    if connection.transport != HostTransport::WebSocket {
        return Err(activity_error(anyhow::anyhow!(
            "unsupported environment job transport: {:?}",
            connection.transport
        )));
    }
    let mut client = HostDataClient::connect(
        &connection.endpoint,
        WebSocketConnectOptions {
            user_agent: Some("lightspeed-environment-job-workflow".to_owned()),
            ..WebSocketConnectOptions::default()
        },
    )
    .await
    .map_err(activity_error)?;
    let response = client
        .initialize(&InitializeParams {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            client_name: "lightspeed-environment-job-workflow".to_owned(),
            scope: connection.scope.clone(),
            resume_connection_id: None,
        })
        .await
        .map_err(activity_error)?;
    if response.protocol_version != CURRENT_PROTOCOL_VERSION {
        return Err(activity_error(anyhow::anyhow!(
            "unsupported host protocol version {}",
            response.protocol_version
        )));
    }
    let capabilities = response.capabilities;
    client
        .initialized(&InitializedParams {})
        .await
        .map_err(activity_error)?;
    Ok((client, capabilities))
}

fn unix_ms() -> Result<i64, ActivityError> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(activity_error)?;
    i64::try_from(duration.as_millis()).map_err(activity_error)
}
