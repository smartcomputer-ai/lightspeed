import { fileURLToPath } from "node:url";
import { bundleWorkflowCode } from "@temporalio/worker";
import { expect, it } from "vitest";

it(
  "bundles the workflow without Node or activity dependencies",
  async () => {
    const workflowsPath = fileURLToPath(new URL("../src/workflows/index.ts", import.meta.url));
    const bundle = await bundleWorkflowCode({ workflowsPath });
    expect(bundle.code.length).toBeGreaterThan(1_000);
    expect(bundle.code).toContain("channelSessionWorkflowV1");
  },
  30_000,
);
