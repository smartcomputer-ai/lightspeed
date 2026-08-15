import { NativeConnection, Worker } from "@temporalio/worker";
import { createFakeDeliveryActivities } from "../activities/index.js";

const address = process.env.TEMPORAL_ADDRESS ?? "localhost:7233";
const namespace = process.env.TEMPORAL_NAMESPACE ?? "default";
const taskQueue = process.env.LIGHTSPEED_CHANNELS_DELIVERY_TASK_QUEUE;

if (taskQueue === undefined || taskQueue.length === 0) {
  throw new TypeError("LIGHTSPEED_CHANNELS_DELIVERY_TASK_QUEUE is required");
}

const connection = await NativeConnection.connect({ address });
const worker = await Worker.create({
  connection,
  namespace,
  taskQueue,
  activities: createFakeDeliveryActivities(),
});

console.log(`channels: fake delivery worker polling ${namespace}/${taskQueue} at ${address}`);
await worker.run();
