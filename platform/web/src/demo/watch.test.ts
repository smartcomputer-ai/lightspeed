import { expect, it } from "vitest";
import { readEventStream } from "@lightspeed-ai/sdk";
import { createDemoStore } from "./fixtures";
import { createDemoRouter } from "./router";

it("streams committed workspace projections and releases the demo timer on cancel", async () => {
  const store = createDemoStore();
  const universe = [...store.universes.values()][0]!;
  const entry = [...universe.workspaces.values()][0]!;
  const response = await createDemoRouter(store).request(`/api/v1/universes/${universe.universe.id}/watch`);
  expect(response.headers.get("content-type")).toBe("text/event-stream");
  const stream = readEventStream(response.body!);
  try {
    expect(JSON.parse((await stream.next()).value!)).toEqual({ method: "watch/reset", params: {} });
    entry.row = { ...entry.row, displayName: "Changed live", revision: entry.row.revision + 1 };
    const event = JSON.parse((await stream.next()).value!);
    expect(event).toEqual({ method: "workspaces/changed", params: { workspace: entry.row } });
    universe.workspaces.delete(entry.row.workspaceId);
    expect(JSON.parse((await stream.next()).value!)).toEqual({ method: "workspaces/removed", params: { workspaceId: entry.row.workspaceId } });
  } finally {
    await stream.return(undefined);
  }
  expect(response.body!.locked).toBe(false);
});
