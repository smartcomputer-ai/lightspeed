export { LightspeedRpcError, LightspeedTransportError, lightspeedRpcErrorKind } from "./errors.js";
export type { LightspeedRpcErrorKind, JsonRpcErrorPayload } from "./errors.js";
export {
  LightspeedClient,
  generateSubmissionId,
} from "./client.js";
export type {
  WatchOptions,
  AwaitRunOptions,
  AwaitRunResult,
  CallOptions,
  LightspeedClientOptions,
  MethodData,
  ReadEventsOptions,
  RequestId,
  SessionEventKindOf,
  SessionEventType,
  StartRunOptions,
  SteerRunOptions,
  TerminalRunStatus,
  TerminalRunView,
} from "./client.js";
export { CLIENT_HELPERS } from "./generated/helpers.js";
export { runTimeline } from "./run-timeline.js";
export type { RunTimelineItem, RunTimelineTool } from "./run-timeline.js";
export {
  METHODS,
  METHOD_INFO,
  NOTIFICATIONS,
} from "./generated/methods.js";
export type {
  Method,
  MethodMap,
  MethodParams,
  MethodResult,
  NotificationMethod,
} from "./generated/methods.js";
export type * from "./generated/types.js";

export { readEventStream } from "./event-stream.js";
