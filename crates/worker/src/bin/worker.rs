use clap::Parser;
use temporalio_common::{telemetry::TelemetryOptions, worker::WorkerTaskTypes};
use temporalio_sdk::{Worker, WorkerOptions};
use temporalio_sdk_core::{CoreRuntime, RuntimeOptions};
use worker::WorkerActivities;
use worker::{
    AgentSessionWorkflow, DEFAULT_TASK_QUEUE, DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET,
    connect_temporal,
};

#[derive(Debug, Parser)]
#[command(name = "worker", about = "Run the Forge agent Temporal worker")]
struct Args {
    #[arg(long, env = "FORGE_TASK_QUEUE", default_value = DEFAULT_TASK_QUEUE)]
    task_queue: String,

    #[arg(long, env = "TEMPORAL_ADDRESS", default_value = DEFAULT_TEMPORAL_TARGET)]
    temporal_target: String,

    #[arg(long, env = "TEMPORAL_NAMESPACE", default_value = DEFAULT_TEMPORAL_NAMESPACE)]
    namespace: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let args = Args::parse();
    let runtime = CoreRuntime::new_assume_tokio(
        RuntimeOptions::builder()
            .telemetry_options(TelemetryOptions::builder().build())
            .build()
            .map_err(|error| anyhow::anyhow!("{error}"))?,
    )?;
    let client = connect_temporal(&args.temporal_target, &args.namespace).await?;
    let activities = WorkerActivities::from_env().await?;
    let worker_options = WorkerOptions::new(args.task_queue)
        .register_workflow::<AgentSessionWorkflow>()
        .register_activities(activities)
        .task_types(WorkerTaskTypes::all())
        .build();
    let mut worker = Worker::new(&runtime, client, worker_options)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    worker.run().await?;
    Ok(())
}
