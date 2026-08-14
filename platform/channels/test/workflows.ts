import { defineQuery, defineSignal, setHandler, condition } from "@temporalio/workflow";
import type { EmissionEnvelope } from "../src/contracts/emissions.js";

export { channelSessionWorkflowV1 } from "../src/workflows/channel-session.js";

const emissionSignal = defineSignal<[EmissionEnvelope]>("deliver_emission");
const holderStateQuery = defineQuery<EmissionEnvelope[]>("holder_state");

export async function testHolderWorkflow(): Promise<never> {
  const emissions: EmissionEnvelope[] = [];
  setHandler(emissionSignal, (emission) => {
    emissions.push(emission);
  });
  setHandler(holderStateQuery, () => [...emissions]);
  for (;;) {
    await condition(() => false);
  }
}
