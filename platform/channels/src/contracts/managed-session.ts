export interface WorkflowEndpointV1 {
  workflowId: string;
  workflowKind: string;
}

export interface EnsureManagedSessionInput {
  universeId: string;
  sessionId: string;
  displayName?: string;
  profileId?: string;
  controller: WorkflowEndpointV1;
}

export interface EnsureManagedSessionResult {
  sessionId: string;
  createdAtMs: number;
}

export interface ReadJsonBlobInput {
  universeId: string;
  blobRef: string;
}

export interface PutJsonBlobInput {
  universeId: string;
  value: unknown;
}

export interface PutJsonBlobResult {
  blobRef: string;
}

export interface StartChannelRunInput {
  universeId: string;
  sessionId: string;
  submissionId: string;
  terminalToken: string;
  items: import("./media.js").ChannelInputItem[];
}

export interface StartChannelRunResult {
  runId: string;
}

export interface AppendChannelContextInput {
  universeId: string;
  sessionId: string;
  entries: Array<{ key: string; item: import("./media.js").ChannelInputItem }>;
}

export interface AppendChannelContextResult {
  contextRevision: number;
  results: Array<{
    key: string;
    status: "applied" | "unchanged";
    activationText?: string;
  }>;
}

export interface RemoveChannelContextInput {
  universeId: string;
  sessionId: string;
  keys: string[];
}

export interface RemoveChannelContextResult {
  contextRevision: number;
  results: Array<{ key: string; status: "removed" | "absent" }>;
}

export interface ReconcileTerminalRunInput {
  universeId: string;
  sessionId: string;
  runId: number;
  status: import("./emissions.js").RunStatus;
}

export type ReconcileTerminalRunResult =
  | { action: "suppress"; reason: "messaging_tool" }
  | { action: "deliver"; text: string };

export interface LightspeedActivities {
  ensureManagedSession(input: EnsureManagedSessionInput): Promise<EnsureManagedSessionResult>;
  readJsonBlob(input: ReadJsonBlobInput): Promise<unknown>;
  putJsonBlob(input: PutJsonBlobInput): Promise<PutJsonBlobResult>;
  startChannelRun(input: StartChannelRunInput): Promise<StartChannelRunResult>;
  appendChannelContext(input: AppendChannelContextInput): Promise<AppendChannelContextResult>;
  removeChannelContext(input: RemoveChannelContextInput): Promise<RemoveChannelContextResult>;
  reconcileTerminalRun(input: ReconcileTerminalRunInput): Promise<ReconcileTerminalRunResult>;
}
