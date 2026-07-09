use environments::{
    EnvironmentInstanceId, EnvironmentInstanceStore, EnvironmentJobGroupId,
    EnvironmentJobGroupStatus, JobHandleStore, UpdateEnvironmentJobGroupStatus,
};
use host_client::{HostDataClient, WebSocketConnectOptions};
use host_protocol::{
    data::{
        handshake::{InitializeParams, InitializedParams},
        jobs::{CancelJobsParams, ReadJobsParams},
    },
    shared::{CURRENT_PROTOCOL_VERSION, HostConnectionSpec, HostTransport},
};
use store_pg::PgStore;
use temporal_workflow::{
    EnvironmentJobCancelActivityRequest, EnvironmentJobPollActivityRequest,
    EnvironmentJobPollActivityResult,
};
use temporalio_sdk::activities::ActivityError;

use super::common::activity_error;

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
    let mut client = initialized_client(&instance.connection).await?;
    let response = client
        .read_jobs(&ReadJobsParams {
            namespace: instance_id.as_str().to_owned(),
            jobs: request.job_ids,
            after_seq: None,
            max_bytes: None,
            include_artifacts: false,
            wait_ms: None,
        })
        .await
        .map_err(activity_error)?;
    let jobs = response
        .jobs
        .into_iter()
        .map(|result| result.summary)
        .collect::<Vec<_>>();
    let terminal = !jobs.is_empty() && jobs.iter().all(|job| job.status.is_terminal());
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
    Ok(EnvironmentJobPollActivityResult { jobs, terminal })
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
    let mut client = initialized_client(&instance.connection).await?;
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
) -> Result<HostDataClient<host_client::WebSocketTransport>, ActivityError> {
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
    client
        .initialized(&InitializedParams {})
        .await
        .map_err(activity_error)?;
    Ok(client)
}

fn unix_ms() -> Result<i64, ActivityError> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(activity_error)?;
    i64::try_from(duration.as_millis()).map_err(activity_error)
}
