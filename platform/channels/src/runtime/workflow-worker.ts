import { fileURLToPath } from "node:url";
import { NativeConnection, Worker } from "@temporalio/worker";
import { CHANNELS_WORKFLOW_TASK_QUEUE } from "../contracts/channel.js";
import { installTemporalMetrics } from "./temporal-metrics.js";

const address = process.env.TEMPORAL_ADDRESS ?? "localhost:7233";
const namespace = process.env.TEMPORAL_NAMESPACE ?? "default";
const taskQueue =
  process.env.LIGHTSPEED_CHANNELS_WORKFLOW_TASK_QUEUE ?? CHANNELS_WORKFLOW_TASK_QUEUE;
const workflowsPath = fileURLToPath(new URL("../workflows/index.ts", import.meta.url));

installTemporalMetrics("channels-workflows", 9_090);
const connection = await NativeConnection.connect({ address });
const worker = await Worker.create({
  connection,
  namespace,
  taskQueue,
  workflowsPath,
});

console.log(`channels: workflow worker polling ${namespace}/${taskQueue} at ${address}`);
await worker.run();
