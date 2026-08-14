import type { WorkflowClient } from "@temporalio/client";
import { describe, expect, it, vi } from "vitest";
import {
  selectChannelBinding,
  planChannelAdmission,
  type ChannelBindingCandidate,
  type ChannelControlPlane,
} from "../src/control-plane/bindings.js";
import type { NormalizedInboundV1 } from "../src/contracts/channel.js";
import { admitInbound } from "../src/ingress/admit.js";

const candidate: ChannelBindingCandidate = {
  bindingId: "123e4567-e89b-42d3-a456-426614174000",
  bindingName: "family",
  channelAccountId: "123e4567-e89b-42d3-a456-426614174001",
  accountProvider: "telegram",
  accountId: "primary",
  universeId: "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f",
  universeName: "Family",
  universeActive: true,
  enabled: true,
  matchScope: "group",
  profileId: "family-chat",
  sessionKey: "family",
  pairingRequired: true,
  pairingCode: "PairCode123",
  paired: true,
  activation: {
    group: "mention",
    triggerPrefixes: ["/ask"],
    batching: { debounceMs: 250, maxWaitMs: 1_000, maxMessages: 4 },
  },
  access: { turn: "members", control: "admins" },
  memberRole: "admin",
};

const inbound: NormalizedInboundV1 = {
  version: 1,
  messageId: "42",
  route: { provider: "telegram", accountId: "primary", chatId: "-100123" },
  senderId: "7",
  senderName: "Lukas",
  timestampMs: 1_700_000_000_000,
  text: "hello",
  isDirect: false,
  mentionedBot: true,
  isReplyToBot: false,
};

describe("Channels binding admission", () => {
  it("selects only active, matching, paired candidates in priority order", () => {
    expect(
      selectChannelBinding([{ ...candidate, paired: false }, candidate], inbound.route, false),
    ).toMatchObject({
      bindingName: "family",
      universeId: candidate.universeId,
      authorization: { turnAllowed: true, controlAllowed: true, memberRole: "admin" },
    });
    expect(selectChannelBinding([candidate], inbound.route, true)).toBeNull();
    expect(selectChannelBinding([{ ...candidate, universeActive: false }], inbound.route, false)).toBeNull();
    expect(
      selectChannelBinding([candidate], { ...inbound.route, accountId: "secondary" }, false),
    ).toBeNull();
  });

  it("resolves the platform binding before signal-with-start", async () => {
    const resolved = selectChannelBinding([candidate], inbound.route, false);
    if (resolved === null) {
      throw new Error("expected fixture binding to resolve");
    }
    const resolver: ChannelControlPlane = {
      resolve: vi.fn(async () => resolved),
      admit: vi.fn(async () => ({ status: "bound" as const, binding: resolved })),
      pairingRequired: vi.fn(async () => false),
    };
    const signalWithStart = vi.fn(async (..._args: unknown[]) => ({ signaledRunId: "run-1" }));
    const client = { signalWithStart } as unknown as WorkflowClient;

    const result = await admitInbound(client, resolver, inbound);

    expect(result).toMatchObject({
      status: "admitted",
      binding: { bindingName: "family" },
      signaledRunId: "run-1",
    });
    expect(signalWithStart).toHaveBeenCalledOnce();
    const options = signalWithStart.mock.calls[0]?.[1];
    expect(options).toMatchObject({
      args: [
        {
          bindingId: candidate.bindingId,
          profileId: "family-chat",
          sessionKey: "family",
          activation: {
            mode: "mention",
            triggerPrefixes: ["/ask"],
            batching: { debounceMs: 250, maxWaitMs: 1_000, maxMessages: 4 },
          },
          access: { turn: "members", control: "admins" },
          initialRoute: inbound.route,
        },
      ],
      signalArgs: [inbound],
    });
  });

  it("does not create a workflow for unbound traffic", async () => {
    const resolver: ChannelControlPlane = {
      resolve: vi.fn(async () => null),
      admit: vi.fn(async () => ({ status: "unbound" as const })),
      pairingRequired: vi.fn(async () => false),
    };
    const signalWithStart = vi.fn();
    const client = { signalWithStart } as unknown as WorkflowClient;
    await expect(admitInbound(client, resolver, inbound)).resolves.toEqual({ status: "unbound" });
    expect(signalWithStart).not.toHaveBeenCalled();
  });

  it("consumes exact pairing codes before any workflow is created", async () => {
    expect(
      planChannelAdmission([{ ...candidate, paired: false }], {
        ...inbound,
        text: "PairCode123",
      }),
    ).toMatchObject({ status: "pair", candidate: { bindingName: "family" } });
    expect(
      planChannelAdmission([{ ...candidate, paired: false }], {
        ...inbound,
        text: "wrong",
      }),
    ).toEqual({ status: "pairing_required" });
    expect(
      planChannelAdmission([{ ...candidate, paired: false }], {
        ...inbound,
        text: "ambient group traffic",
        mentionedBot: false,
      }),
    ).toEqual({ status: "pairing_pending" });
  });

  it("returns pairing responses without signaling Temporal", async () => {
    const resolver: ChannelControlPlane = {
      resolve: vi.fn(async () => null),
      admit: vi.fn(async () => ({ status: "pairing_required" as const })),
      pairingRequired: vi.fn(async () => true),
    };
    const signalWithStart = vi.fn();
    const client = { signalWithStart } as unknown as WorkflowClient;
    await expect(admitInbound(client, resolver, inbound)).resolves.toEqual({
      status: "pairing_required",
    });
    expect(signalWithStart).not.toHaveBeenCalled();
  });
});
