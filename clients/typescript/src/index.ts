export { LightspeedApiError, LightspeedTransportError } from "./errors.js";
export { prepareHttpRequest } from "./http-request.js";
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
  ReadEventsOptions,
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
export { transcriptAuthor, transcriptMetadata } from "./transcript-metadata.js";
export type { TranscriptMessageAuthor, TranscriptMessageMetadata } from "./transcript-metadata.js";
export {
  OPERATIONS,
  OPERATION_INFO,
  NOTIFICATIONS,
} from "./generated/operations.js";
export type {
  OperationId,
  OperationMap,
  OperationParams,
  OperationResult,
  NotificationOperationId,
} from "./generated/operations.js";
export type * from "./generated/types.js";

export { readEventStream } from "./event-stream.js";
export type { ServerSentEvent } from "./event-stream.js";
