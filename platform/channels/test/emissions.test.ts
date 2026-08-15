import { describe, expect, it } from "vitest";
import {
  joinedReplyPromiseId,
  parseEmissionEnvelope,
  sourceResolutionEmissionId,
  sourceResolutionEnvelope,
  type EmissionEnvelope,
} from "../src/contracts/emissions.js";

const universeId = "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f";
const invocationId = `wti:sha256:${"a".repeat(64)}`;

function invocationEnvelope(): EmissionEnvelope {
  return {
    emission_id: invocationId,
    producer: {
      kind: "session",
      universe_id: universeId,
      session_id: "channel:v1:telegram:test",
      log_seq: 42,
    },
    body: {
      kind: "tool_invocation",
      invocation: {
        invocation_id: invocationId,
        tool_id: "channels.message_send.v1",
        semantic_type: "channels.message.send.v1",
        schema_revision: 1,
        binding_fingerprint: "binding:v1:test",
        session_universe_id: universeId,
        session_id: "channel:v1:telegram:test",
        run_id: 7,
        turn_id: 8,
        tool_batch_id: 9,
        tool_call_id: "call_1",
        arguments_ref: `sha256:${"c".repeat(64)}`,
        completion_promises: {
          reply: `wtp:sha256:${"d".repeat(64)}`,
        },
      },
    },
  };
}

describe("Lightspeed emission contract", () => {
  it("accepts the Rust Serde shape for a pushed invocation", () => {
    const envelope = invocationEnvelope();
    expect(parseEmissionEnvelope(envelope)).toEqual(envelope);
  });

  it("extracts only the reserved runtime-owned Joined reply", () => {
    const envelope = invocationEnvelope();
    if (envelope.body.kind !== "tool_invocation") {
      throw new TypeError("expected tool invocation fixture");
    }
    const invocation = envelope.body.invocation;
    expect(joinedReplyPromiseId(invocation)).toBe(
      `wtp:sha256:${"d".repeat(64)}`,
    );
    const { completion_promises: _completionPromises, ...withoutCompletion } = invocation;
    expect(() => joinedReplyPromiseId(withoutCompletion)).toThrow(/exactly/);
    expect(() =>
      joinedReplyPromiseId({
        ...invocation,
        completion_promises: { other: `wtp:sha256:${"d".repeat(64)}` },
      }),
    ).toThrow(/reserved `reply`/);
    expect(() =>
      joinedReplyPromiseId({
        ...invocation,
        completion_promises: {
          reply: `wtp:sha256:${"d".repeat(64)}`,
          extra: `wtp:sha256:${"e".repeat(64)}`,
        },
      }),
    ).toThrow(/exactly/);
  });

  it("rejects malformed signal payloads at the workflow boundary", () => {
    const envelope = invocationEnvelope();
    const malformed = {
      ...envelope,
      body: {
        ...envelope.body,
        invocation: { ...(envelope.body as { invocation: object }).invocation, run_id: "7" },
      },
    };
    expect(() => parseEmissionEnvelope(malformed)).toThrow(/run_id/);
    expect(() =>
      parseEmissionEnvelope({ ...envelope, body: { kind: "future_protocol_shape" } }),
    ).toThrow(/unknown emission body kind/);
  });

  it("matches the Rust source-resolution hash framing", () => {
    const promiseId = `wtp:sha256:${"b".repeat(64)}`;
    expect(sourceResolutionEmissionId(universeId, "lightspeed.channels.v1/test", promiseId)).toBe(
      "emission:sha256:97b9cfa7f3a3197a3acbc05e045d84633bd626fe6ccc5b604b2cf6d283d6532c",
    );

    const envelope = sourceResolutionEnvelope({
      universeId,
      producerWorkflowId: "lightspeed.channels.v1/test",
      promiseId,
      resolution: { kind: "resolved", payload_ref: null },
    });
    expect(parseEmissionEnvelope(envelope)).toEqual(envelope);
  });
});
