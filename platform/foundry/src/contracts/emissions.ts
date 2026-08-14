import { sha256 } from "@noble/hashes/sha2.js";

const EMISSION_ID = /^(?:emission|wti):sha256:[0-9a-f]{64}$/;
const INVOCATION_ID = /^wti:sha256:[0-9a-f]{64}$/;
const BLOB_REF = /^sha256:[0-9a-f]{64}$/;

export const DELIVER_EMISSION_SIGNAL = "deliver_emission";

export type RunStatus =
  | "active"
  | "parked"
  | "cancelling"
  | "cancelling_grace"
  | "completed"
  | "failed"
  | "cancelled";

export interface WorkflowToolInvocation {
  invocation_id: string;
  tool_id: string;
  semantic_type: string;
  schema_revision: number;
  binding_fingerprint: string;
  session_universe_id: string;
  session_id: string;
  run_id: number;
  turn_id: number;
  tool_batch_id: number;
  tool_call_id: string;
  arguments_ref: string;
  /**
   * Receiver-visible completion mapping. Joined invocations carry exactly the
   * runtime-owned `reply` Promise; Promises invocations carry one entry per
   * declared completion key.
   */
  completion_promises?: Record<string, string>;
}

export type EmissionProducer =
  | {
      kind: "session";
      universe_id: string;
      session_id: string;
      log_seq: number;
    }
  | {
      kind: "workflow";
      universe_id: string;
      workflow_id: string;
    };

export type PromiseResolution =
  | { kind: "resolved"; payload_ref: string | null }
  | { kind: "failed"; error_ref: string | null }
  | { kind: "cancelled" };

export type EmissionBody =
  | {
      kind: "run_terminal";
      token: string;
      run_id: number;
      status: RunStatus;
      output_ref: string | null;
      failure_message_ref: string | null;
    }
  | {
      kind: "source_resolution";
      promise_id: string;
      resolution: PromiseResolution;
    }
  | {
      kind: "tool_invocation";
      invocation: WorkflowToolInvocation;
    }
  | {
      kind: "invocation_cancellation";
      invocation_id: string;
      completion_key: string;
      promise_id: string;
    };

export interface EmissionEnvelope {
  emission_id: string;
  producer: EmissionProducer;
  body: EmissionBody;
}

/**
 * Decode the fixed P100/P106 signal payload at the workflow boundary.
 *
 * These names and tagged unions mirror Lightspeed's Serde contract in
 * `engine::emission`; accepting unknown shapes here would turn a transport
 * mismatch into silently lost pack work.
 */
export function parseEmissionEnvelope(value: unknown): EmissionEnvelope {
  const envelope = record(value, "emission envelope");
  requirePattern(envelope, "emission_id", EMISSION_ID);
  parseProducer(envelope.producer);
  parseBody(envelope.body);
  return value as EmissionEnvelope;
}

export interface SourceResolutionEnvelopeInput {
  universeId: string;
  producerWorkflowId: string;
  promiseId: string;
  resolution: PromiseResolution;
}

/** Build the exact envelope a workflow sends back to a Lightspeed holder. */
export function sourceResolutionEnvelope(
  input: SourceResolutionEnvelopeInput,
): EmissionEnvelope {
  requireNonEmpty(input.universeId, "universeId");
  requireNonEmpty(input.producerWorkflowId, "producerWorkflowId");
  requireNonEmpty(input.promiseId, "promiseId");
  parseResolution(input.resolution);

  return {
    emission_id: sourceResolutionEmissionId(
      input.universeId,
      input.producerWorkflowId,
      input.promiseId,
    ),
    producer: {
      kind: "workflow",
      universe_id: input.universeId,
      workflow_id: input.producerWorkflowId,
    },
    body: {
      kind: "source_resolution",
      promise_id: input.promiseId,
      resolution: input.resolution,
    },
  };
}

/**
 * Port of `EmissionId::for_source_resolution` and `update_digest_part`.
 * Lengths are unsigned 64-bit big-endian values, matching Rust exactly.
 */
export function sourceResolutionEmissionId(
  universeId: string,
  producerWorkflowId: string,
  promiseId: string,
): string {
  const digest = sha256.create();
  for (const part of [
    "lightspeed.emission.v1",
    "source_resolution",
    universeId,
    producerWorkflowId,
    promiseId,
  ]) {
    const bytes = new TextEncoder().encode(part);
    digest.update(u64be(bytes.length));
    digest.update(bytes);
  }
  return `emission:sha256:${hex(digest.digest())}`;
}

/**
 * Return the single receiver-visible completion Promise for tools declared
 * with the reserved `reply` key (Joined tools and reply-keyed Promises tools).
 */
export function replyPromiseId(invocation: WorkflowToolInvocation): string {
  const promises = invocation.completion_promises;
  if (promises === undefined) {
    throw new TypeError(
      "pushed invocation must carry exactly the reserved `reply` completion promise",
    );
  }
  const keys = Object.keys(promises);
  if (keys.length !== 1 || keys[0] !== "reply") {
    throw new TypeError(
      "pushed invocation must carry exactly the reserved `reply` completion promise",
    );
  }
  const promiseId = promises.reply;
  requireNonEmptyValue(promiseId, "reply promise");
  return promiseId;
}

function parseProducer(value: unknown): void {
  const producer = record(value, "emission producer");
  const kind = requireString(producer, "kind");
  requireNonEmpty(requireString(producer, "universe_id"), "producer.universe_id");
  if (kind === "session") {
    requireNonEmpty(requireString(producer, "session_id"), "producer.session_id");
    requireSafeInteger(producer, "log_seq");
    return;
  }
  if (kind === "workflow") {
    requireNonEmpty(requireString(producer, "workflow_id"), "producer.workflow_id");
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
      requireOptionalBlobRef(body, "output_ref");
      requireOptionalBlobRef(body, "failure_message_ref");
      return;
    case "source_resolution":
      requireNonEmpty(requireString(body, "promise_id"), "source_resolution.promise_id");
      parseResolution(body.resolution);
      return;
    case "tool_invocation":
      parseInvocation(body.invocation);
      return;
    case "invocation_cancellation":
      requirePattern(body, "invocation_id", INVOCATION_ID);
      requireNonEmpty(requireString(body, "completion_key"), "cancellation.completion_key");
      requireNonEmpty(requireString(body, "promise_id"), "cancellation.promise_id");
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
    "session_universe_id",
    "session_id",
    "tool_call_id",
  ]) {
    requireNonEmpty(requireString(invocation, key), `invocation.${key}`);
  }
  requirePattern(invocation, "arguments_ref", BLOB_REF);
  for (const key of ["schema_revision", "run_id", "turn_id", "tool_batch_id"]) {
    requireSafeInteger(invocation, key);
  }
  if (invocation.completion_promises !== undefined) {
    const promises = record(invocation.completion_promises, "completion promises");
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

function requireOptionalBlobRef(value: Record<string, unknown>, key: string): void {
  const field = value[key];
  if (field !== null) {
    requireNonEmptyValue(field, key);
    if (!BLOB_REF.test(field)) {
      throw new TypeError(`${key} is not a canonical blob ref`);
    }
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
  if (typeof field !== "string") {
    throw new TypeError(`${key} must be a string`);
  }
  return field;
}

function requirePattern(value: Record<string, unknown>, key: string, pattern: RegExp): void {
  const field = requireString(value, key);
  if (!pattern.test(field)) {
    throw new TypeError(`${key} has an invalid format`);
  }
}

function requireSafeInteger(value: Record<string, unknown>, key: string): void {
  const field = value[key];
  if (!Number.isSafeInteger(field) || (field as number) < 0) {
    throw new TypeError(`${key} must be a non-negative safe integer`);
  }
}

function requireNonEmpty(value: string, name: string): void {
  if (value.length === 0) {
    throw new TypeError(`${name} must not be empty`);
  }
}

function requireNonEmptyValue(value: unknown, name: string): asserts value is string {
  if (typeof value !== "string" || value.length === 0) {
    throw new TypeError(`${name} must be a non-empty string`);
  }
}

function u64be(value: number): Uint8Array {
  const bytes = new Uint8Array(8);
  new DataView(bytes.buffer).setBigUint64(0, BigInt(value), false);
  return bytes;
}

function hex(value: Uint8Array): string {
  return Array.from(value, (byte) => byte.toString(16).padStart(2, "0")).join("");
}
