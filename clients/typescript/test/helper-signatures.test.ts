import { expectTypeOf, it } from "vitest";
import { LightspeedClient, type AgentNotification, type BlobPutResult, type InputItem, type ReadEventsOptions, type RunStartParams, type RunStartResponse, type RunStartSource, type RunSteerParams, type SessionEventsReadParams, type StartRunOptions, type SteerRunOptions, type UniverseWatchParams } from "../src/index.js";

// These relationships ensure future wire fields cannot disappear from handwritten
// helper options while generated helpers remain directly derived from the DTOs.
it("preserves the full handwritten wire surface and ergonomic inputs", () => {
  expectTypeOf<Pick<StartRunOptions, keyof Omit<RunStartParams, "sessionId" | "source">>>().toEqualTypeOf<Omit<RunStartParams, "sessionId" | "source">>();
  expectTypeOf<Pick<SteerRunOptions, keyof Omit<RunSteerParams, "sessionId" | "runId" | "items">>>().toEqualTypeOf<Omit<RunSteerParams, "sessionId" | "runId" | "items">>();
  expectTypeOf<Pick<ReadEventsOptions, keyof Omit<SessionEventsReadParams, "sessionId">>>().toEqualTypeOf<Omit<SessionEventsReadParams, "sessionId">>();
  expectTypeOf<Parameters<LightspeedClient["startRun"]>[1]>().toEqualTypeOf<string | InputItem[] | RunStartSource>();
  expectTypeOf<ReturnType<LightspeedClient["startRun"]>>().toEqualTypeOf<Promise<RunStartResponse>>();
  expectTypeOf<ReturnType<LightspeedClient["uploadBlob"]>>().toEqualTypeOf<Promise<BlobPutResult>>();
  expectTypeOf<ReturnType<LightspeedClient["readBlobBytes"]>>().toEqualTypeOf<Promise<Uint8Array>>();
  expectTypeOf<ReturnType<LightspeedClient["readWorkspaceFile"]>>().toEqualTypeOf<Promise<Uint8Array>>();
  expectTypeOf<Parameters<LightspeedClient["watch"]>[0]>().toEqualTypeOf<UniverseWatchParams | undefined>();
  expectTypeOf<ReturnType<LightspeedClient["watch"]>>().toExtend<AsyncGenerator<AgentNotification>>();
});
