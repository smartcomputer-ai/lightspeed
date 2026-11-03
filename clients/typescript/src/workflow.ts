import { sha256 } from "@noble/hashes/sha2.js";

import { WORKFLOW_CONTRACT_MANIFEST } from "./generated/workflow-manifest.js";
import type {
  EmissionEnvelope,
  PromiseResolution,
  RunStatus,
  WorkflowToolInvocation,
} from "./generated/workflow-types.js";

export type * from "./generated/workflow-types.js";

export const DELIVER_EMISSION_SIGNAL =
  WORKFLOW_CONTRACT_MANIFEST.signals.deliverEmission;
export const WORKFLOW_TOOL_RECOVERY_QUERY =
  WORKFLOW_CONTRACT_MANIFEST.queries.workflowToolRecovery;
export const WORKFLOW_TOOL_EXECUTION_KIND =
  WORKFLOW_CONTRACT_MANIFEST.workflowTools.executionKind;
export const WORKFLOW_TOOL_RECIPE_FORMAT_V1 =
  WORKFLOW_CONTRACT_MANIFEST.workflowTools.recipeFormatV1;
export const WORKFLOW_TOOL_RECIPE_FINGERPRINT_PREFIX =
  WORKFLOW_CONTRACT_MANIFEST.workflowTools.recipeFingerprintPrefix;
export const REPLY_COMPLETION_KEY =
  WORKFLOW_CONTRACT_MANIFEST.workflowTools.replyCompletionKey;

/** Activity names a connector host registers on its account task queues. */
export const CHANNEL_CONNECTOR_ACTIVITIES =
  WORKFLOW_CONTRACT_MANIFEST.channels.connectorActivities;
export const CHANNEL_CONVERSATION_WORKFLOW_KIND =
  WORKFLOW_CONTRACT_MANIFEST.channels.workflowKind;
export const CHANNEL_INBOUND_SIGNAL =
  WORKFLOW_CONTRACT_MANIFEST.channels.inboundSignal;
export const CHANNEL_STATE_QUERY =
  WORKFLOW_CONTRACT_MANIFEST.channels.stateQuery;
export const CHANNEL_DELIVERY_RECEIPT_SIGNAL =
  WORKFLOW_CONTRACT_MANIFEST.channels.deliveryReceiptSignal;

/** Known-answer vectors emitted by Rust and shared by every generated consumer. */
export const WORKFLOW_CONTRACT_VECTORS = WORKFLOW_CONTRACT_MANIFEST.vectors;

const EMISSION_ID = /^(?:emission|wti):sha256:[0-9a-f]{64}$/;
const INVOCATION_ID = /^wti:sha256:[0-9a-f]{64}$/;
const BLOB_REF = /^sha256:[0-9a-f]{64}$/;
const PROMISE_ID = /^promise_(?:0|[1-9][0-9]{0,19})$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const SESSION_ID = /^[A-Za-z0-9][A-Za-z0-9_.:-]*$/;
const textEncoder = new TextEncoder();

export const emissionId = {
  runTerminal(
    universeId: string,
    sessionId: string,
    runId: number,
    token: string,
  ): string {
    requireSessionId(sessionId);
    requireSafeIntegerValue(runId, "runId");
    requireNonEmpty(token, "token");
    return derivedEmissionId("run_terminal", [
      utf8(normalizeUniverseId(universeId)),
      utf8(sessionId),
      u64be(runId),
      utf8(token),
    ]);
  },

  /**
   * Promise ids are session-scoped counters (`promise_7`), so the holder
   * workflow id is part of the emission identity: one producer resolving
   * `promise_7` for two holders sends two distinct emissions.
   */
  sourceResolution(
    universeId: string,
    producerWorkflowId: string,
    holderWorkflowId: string,
    promiseId: string,
  ): string {
    requireNonEmpty(producerWorkflowId, "producerWorkflowId");
    requireNonEmpty(holderWorkflowId, "holderWorkflowId");
    requireNonEmpty(promiseId, "promiseId");
    return derivedEmissionId("source_resolution", [
      utf8(normalizeUniverseId(universeId)),
      utf8(producerWorkflowId),
      utf8(holderWorkflowId),
      utf8(promiseId),
    ]);
  },

  toolInvocation(invocationId: string): string {
    requireCanonicalInvocationId(invocationId);
    return invocationId;
  },

  invocationCancellation(invocationId: string, completionKey: string): string {
    requireCanonicalInvocationId(invocationId);
    requireNonEmpty(completionKey, "completionKey");
    return derivedEmissionId("invocation_cancellation", [
      utf8(invocationId),
      utf8(completionKey),
    ]);
  },
} as const;

export interface SourceResolutionEnvelopeInput {
  universeId: string;
  producerWorkflowId: string;
  /** The Lightspeed session workflow that holds the promise. */
  holderWorkflowId: string;
  promiseId: string;
  resolution: PromiseResolution;
}

/** Build the exact envelope a workflow sends back to a Lightspeed holder. */
export function sourceResolutionEnvelope(
  input: SourceResolutionEnvelopeInput,
): EmissionEnvelope {
  const universeId = normalizeUniverseId(input.universeId);
  requireNonEmpty(input.producerWorkflowId, "producerWorkflowId");
  requireNonEmpty(input.holderWorkflowId, "holderWorkflowId");
  // The holder logs and drops an envelope it cannot decode, so this is the only
  // place the plugin can hear about a bad id.
  if (!PROMISE_ID.test(input.promiseId)) {
    throw new TypeError("promiseId has an invalid format");
  }
  parseResolution(input.resolution);
  return {
    emission_id: emissionId.sourceResolution(
      universeId,
      input.producerWorkflowId,
      input.holderWorkflowId,
      input.promiseId,
    ),
    producer: {
      kind: "workflow",
      universe_id: universeId,
      workflow_id: input.producerWorkflowId,
    },
    body: {
      kind: "source_resolution",
      promise_id: input.promiseId,
      resolution: input.resolution,
    },
  };
}

/** Decode and validate the fixed signal payload at a workflow boundary. */
export function parseEmissionEnvelope(value: unknown): EmissionEnvelope {
  const envelope = record(value, "emission envelope");
  requirePattern(envelope, "emission_id", EMISSION_ID);
  parseProducer(envelope.producer);
  parseBody(envelope.body);
  return value as EmissionEnvelope;
}

/** Return the single receiver-visible Promise under the reserved `reply` key. */
export function replyPromiseId(invocation: WorkflowToolInvocation): string {
  const promises = invocation.completion_promises;
  if (promises === undefined || promises === null) {
    throw new TypeError(
      "pushed invocation must carry exactly the reserved `reply` completion promise",
    );
  }
  const keys = Object.keys(promises);
  if (keys.length !== 1 || keys[0] !== REPLY_COMPLETION_KEY) {
    throw new TypeError(
      "pushed invocation must carry exactly the reserved `reply` completion promise",
    );
  }
  const promiseId = promises[REPLY_COMPLETION_KEY];
  requireNonEmptyValue(promiseId, "reply promise");
  return promiseId;
}

/** Compose the stable Temporal workflow id of a Lightspeed session. */
export function sessionWorkflowId(
  universeId: string,
  sessionId: string,
): string {
  requireSessionId(sessionId);
  return `${normalizeUniverseId(universeId)}/${sessionId}`;
}

/** Compose the stable Temporal workflow id of an environment job group. */
export function environmentJobWorkflowId(
  universeId: string,
  environmentId: string,
  jobGroupId: string,
): string {
  requireNonEmpty(environmentId, "environmentId");
  requireNonEmpty(jobGroupId, "jobGroupId");
  return `${normalizeUniverseId(universeId)}/envjob-${environmentId}-${jobGroupId}`;
}

/**
 * Task queue of the connector host serving one channel account:
 * `lightspeed-connector-{provider}-{24 hex}`, the hex being sha256 over the
 * length-prefixed domain, hyphenated lowercase universe id, provider, and
 * account id. The core routes connector activities here; the host polls it.
 */
export function connectorTaskQueue(
  universeId: string,
  provider: string,
  accountId: string,
): string {
  requireNonEmpty(provider, "provider");
  requireNonEmpty(accountId, "accountId");
  const digest = sha256.create();
  for (const part of [
    utf8(WORKFLOW_CONTRACT_MANIFEST.channels.domains.connectorTaskQueue),
    utf8(normalizeUniverseId(universeId)),
    utf8(provider),
    utf8(accountId),
  ]) {
    digest.update(u64be(part.length));
    digest.update(part);
  }
  return `lightspeed-connector-${provider}-${hex(digest.digest()).slice(0, 24)}`;
}

/** Split a canonical session workflow id, returning undefined for other ids. */
export function splitWorkflowId(
  workflowId: string,
): { universeId: string; sessionId: string } | undefined {
  const separator = workflowId.indexOf("/");
  if (separator < 0 || workflowId.indexOf("/", separator + 1) >= 0)
    return undefined;
  const universeId = workflowId.slice(0, separator);
  const sessionId = workflowId.slice(separator + 1);
  try {
    const normalizedUniverseId = normalizeUniverseId(universeId);
    requireSessionId(sessionId);
    return { universeId: normalizedUniverseId, sessionId };
  } catch {
    return undefined;
  }
}

/** Fingerprint the exact raw recipe bytes with the canonical prefix. */
export function recipeFingerprint(recipe: string | Uint8Array): string {
  const bytes = typeof recipe === "string" ? utf8(recipe) : recipe;
  return `${WORKFLOW_TOOL_RECIPE_FINGERPRINT_PREFIX}${hex(sha256(bytes))}`;
}

function derivedEmissionId(kind: string, parts: Uint8Array[]): string {
  const digest = sha256.create();
  for (const part of [
    utf8(WORKFLOW_CONTRACT_MANIFEST.emissionIds.hashDomain),
    utf8(kind),
    ...parts,
  ]) {
    digest.update(u64be(part.length));
    digest.update(part);
  }
  return `${WORKFLOW_CONTRACT_MANIFEST.emissionIds.prefix}${hex(digest.digest())}`;
}

function parseProducer(value: unknown): void {
  const producer = record(value, "emission producer");
  const kind = requireString(producer, "kind");
  normalizeUniverseId(requireString(producer, "universe_id"));
  if (kind === "session") {
    requireSessionId(requireString(producer, "session_id"));
    requireSafeInteger(producer, "log_seq");
    return;
  }
  if (kind === "workflow") {
    requireNonEmpty(
      requireString(producer, "workflow_id"),
      "producer.workflow_id",
    );
    return;
  }
  throw new TypeError(`unknown emission producer kind: ${kind}`);
}

function parseBody(value: unknown): void {
  const body = record(value, "emission body");
  const kind = requireString(body, "kind");
  switch (kind) {
    case "run_terminal":
      requireNonEmpty(requireString(body, "token"), "run_terminal.token");
      requireSafeInteger(body, "run_id");
      requireRunStatus(body.status);
      if (body.output !== undefined && body.output !== null) {
        const output = record(body.output, "run_terminal.output");
        requireString(output, "content_ref");
        requireOptionalBlobRef(output, "content_ref");
        for (const key of ["media_type", "provider_kind"]) {
          if (output[key] !== undefined && output[key] !== null) requireString(output, key);
        }
      }
      requireOptionalBlobRef(body, "failure_message_ref");
      return;
    case "source_resolution":
      requirePattern(body, "promise_id", PROMISE_ID);
      parseResolution(body.resolution);
      return;
    case "tool_invocation":
      parseInvocation(body.invocation);
      requireNonEmpty(
        requireString(body, "holder_workflow_id"),
        "tool_invocation.holder_workflow_id",
      );
      return;
    case "invocation_cancellation":
      requirePattern(body, "invocation_id", INVOCATION_ID);
      requireNonEmpty(
        requireString(body, "completion_key"),
        "cancellation.completion_key",
      );
      requirePattern(body, "promise_id", PROMISE_ID);
      return;
    default:
      throw new TypeError(`unknown emission body kind: ${kind}`);
  }
}

function parseInvocation(value: unknown): void {
  const invocation = record(value, "workflow tool invocation");
  requirePattern(invocation, "invocation_id", INVOCATION_ID);
  for (const key of [
    "tool_id",
    "semantic_type",
    "binding_fingerprint",
    "session_id",
    "tool_call_id",
  ]) {
    requireNonEmpty(requireString(invocation, key), `invocation.${key}`);
  }
  normalizeUniverseId(requireString(invocation, "session_universe_id"));
  requirePattern(invocation, "arguments_ref", BLOB_REF);
  const contextRef = invocation.execution_context_ref;
  if (contextRef !== undefined && contextRef !== null) {
    requirePattern(invocation, "execution_context_ref", BLOB_REF);
  }
  for (const key of ["schema_revision", "run_id", "turn_id", "tool_batch_id"]) {
    requireSafeInteger(invocation, key);
  }
  if (
    invocation.completion_promises !== undefined &&
    invocation.completion_promises !== null
  ) {
    const promises = record(
      invocation.completion_promises,
      "completion promises",
    );
    for (const [key, promiseId] of Object.entries(promises)) {
      requireNonEmpty(key, "completion promise key");
      requireNonEmptyValue(promiseId, `completion promise ${key}`);
    }
  }
}

function parseResolution(value: unknown): void {
  const resolution = record(value, "promise resolution");
  const kind = requireString(resolution, "kind");
  if (kind === "resolved") {
    requireOptionalBlobRef(resolution, "payload_ref");
    return;
  }
  if (kind === "failed") {
    requireOptionalBlobRef(resolution, "error_ref");
    return;
  }
  if (kind !== "cancelled") {
    throw new TypeError(`unknown promise resolution kind: ${kind}`);
  }
}

function requireRunStatus(value: unknown): asserts value is RunStatus {
  if (
    value !== "active" &&
    value !== "parked" &&
    value !== "cancelling" &&
    value !== "cancelling_grace" &&
    value !== "completed" &&
    value !== "failed" &&
    value !== "cancelled"
  ) {
    throw new TypeError(`invalid run status: ${String(value)}`);
  }
}

function requireOptionalBlobRef(
  value: Record<string, unknown>,
  key: string,
): void {
  const field = value[key];
  if (field !== undefined && field !== null) {
    requireNonEmptyValue(field, key);
    if (!BLOB_REF.test(field))
      throw new TypeError(`${key} is not a canonical blob ref`);
  }
}

function record(value: unknown, name: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError(`${name} must be an object`);
  }
  return value as Record<string, unknown>;
}

function requireString(value: Record<string, unknown>, key: string): string {
  const field = value[key];
  if (typeof field !== "string") throw new TypeError(`${key} must be a string`);
  return field;
}

function requirePattern(
  value: Record<string, unknown>,
  key: string,
  pattern: RegExp,
): void {
  const field = requireString(value, key);
  if (!pattern.test(field)) throw new TypeError(`${key} has an invalid format`);
}

function requireSafeInteger(value: Record<string, unknown>, key: string): void {
  requireSafeIntegerValue(value[key], key);
}

function requireSafeIntegerValue(
  value: unknown,
  name: string,
): asserts value is number {
  if (!Number.isSafeInteger(value) || (value as number) < 0) {
    throw new TypeError(`${name} must be a non-negative safe integer`);
  }
}

function requireSessionId(value: string): void {
  if (!SESSION_ID.test(value) || utf8(value).length > 128) {
    throw new TypeError("invalid session id");
  }
}

function normalizeUniverseId(value: string): string {
  if (!UUID.test(value)) throw new TypeError("universeId must be a UUID");
  return value.toLowerCase();
}

function requireCanonicalInvocationId(value: string): void {
  if (!INVOCATION_ID.test(value))
    throw new TypeError("invalid workflow tool invocation id");
}

function requireNonEmpty(value: string, name: string): void {
  if (value.length === 0) throw new TypeError(`${name} must not be empty`);
}

function requireNonEmptyValue(
  value: unknown,
  name: string,
): asserts value is string {
  if (typeof value !== "string" || value.length === 0) {
    throw new TypeError(`${name} must be a non-empty string`);
  }
}

function utf8(value: string): Uint8Array {
  return textEncoder.encode(value);
}

function u64be(value: number): Uint8Array {
  const bytes = new Uint8Array(8);
  new DataView(bytes.buffer).setBigUint64(0, BigInt(value), false);
  return bytes;
}

function hex(value: Uint8Array): string {
  return Array.from(value, (byte) => byte.toString(16).padStart(2, "0")).join(
    "",
  );
}
