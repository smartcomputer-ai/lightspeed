import { NativeConnection, Worker } from "@temporalio/worker";
import { createDb } from "@lightspeed/platform-db";
import {
  createFoundryControlPlaneActivities,
  createFoundryLightspeedActivities,
} from "../activities/index.js";
import { FOUNDRY_ACTIVITY_TASK_QUEUE } from "../contracts/foundry.js";

const address = process.env.TEMPORAL_ADDRESS ?? "localhost:7233";
const namespace = process.env.TEMPORAL_NAMESPACE ?? "default";
const taskQueue = process.env.FOUNDRY_ACTIVITY_TASK_QUEUE ?? FOUNDRY_ACTIVITY_TASK_QUEUE;
const endpoint = process.env.LIGHTSPEED_ENDPOINT;
const databaseUrl = process.env.LIGHTSPEED_PLATFORM_DATABASE_URL;

if (endpoint === undefined || endpoint.length === 0) {
  throw new TypeError("LIGHTSPEED_ENDPOINT is required");
}
if (databaseUrl === undefined || databaseUrl.length === 0) {
  throw new TypeError("LIGHTSPEED_PLATFORM_DATABASE_URL is required");
}

const database = createDb(databaseUrl);
const connection = await NativeConnection.connect({ address });
const worker = await Worker.create({
  connection,
  namespace,
  taskQueue,
  activities: {
    ...createFoundryLightspeedActivities({ endpoint }),
    ...createFoundryControlPlaneActivities(database.db),
  },
});

console.log(`foundry: activity worker polling ${namespace}/${taskQueue} at ${address}`);
try {
  await worker.run();
} finally {
  await database.pool.end();
}
