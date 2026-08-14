import {
  LightspeedClient,
  type ContextEntryView,
  type ManagedSessionStartParams,
  type SessionView,
} from "@lightspeed/agent-client";
import type {
  EnsureManagedSessionInput,
  EnsureManagedSessionResult,
  LightspeedActivities,
} from "../contracts/managed-session.js";
import {
  CHANNEL_TOOL_DESCRIPTIONS,
  CHANNEL_TOOL_SCHEMAS,
  channelWorkflowTools,
  type ChannelToolDescriptionName,
  type ChannelToolDescriptionRefs,
  type ChannelToolSchemaName,
  type ChannelToolSchemaRefs,
} from "../contracts/tools.js";

export interface LightspeedActivityConfig {
  endpoint: string;
  fetch?: typeof fetch;
}

export function createLightspeedActivities(config: LightspeedActivityConfig): LightspeedActivities {
  return {
    async ensureManagedSession(input) {
      const client = clientForUniverse(config, input.universeId);
      const toolRefs = await putToolAssets(client);
      const params: ManagedSessionStartParams = {
        sessionId: input.sessionId,
        workflowTools: {
          version: 1,
          lifecycleController: input.controller,
          tools: channelWorkflowTools(input.controller, toolRefs.schemas, toolRefs.descriptions),
        },
      };
      if (input.displayName !== undefined) {
        params.displayName = input.displayName;
      }
      if (input.profileId !== undefined) {
        params.profile = { kind: "named", profileId: input.profileId };
      }

      const response = await client.call("session/managed/start", params);
      return {
        sessionId: response.result.session.id,
        createdAtMs: response.result.session.createdAtMs,
      };
    },
    async readJsonBlob(input) {
      const client = clientForUniverse(config, input.universeId);
      const response = await client.call("blobs/read", { blobRef: input.blobRef });
      const bytes = Buffer.from(response.result.bytesBase64, "base64");
      if (bytes.byteLength !== response.result.bytes) {
        throw new TypeError(
          `blobs/read returned ${bytes.byteLength} bytes, expected ${response.result.bytes}`,
        );
      }
      return JSON.parse(bytes.toString("utf8")) as unknown;
    },
    async putJsonBlob(input) {
      const client = clientForUniverse(config, input.universeId);
      const response = await client.call("blobs/put", {
        blobs: [
          {
            bytesBase64: Buffer.from(JSON.stringify(input.value), "utf8").toString("base64"),
          },
        ],
      });
      const blob = response.result.blobs?.[0];
      if (blob === undefined) {
        throw new Error("blobs/put omitted JSON blob result");
      }
      return { blobRef: blob.blobRef };
    },
    async startChannelRun(input) {
      if (input.items.length === 0) {
        throw new TypeError("startChannelRun requires at least one input item");
      }
      const client = clientForUniverse(config, input.universeId);
      const response = await client.call("session/runs/start", {
        sessionId: input.sessionId,
        source: { type: "input", items: input.items },
        submissionId: input.submissionId,
        notifyOnTerminal: { token: input.terminalToken },
      });
      return { runId: response.result.run.id };
    },
    async appendChannelContext(input) {
      if (input.entries.length === 0) {
        throw new TypeError("appendChannelContext requires at least one entry");
      }
      const client = clientForUniverse(config, input.universeId);
      const response = await client.call("session/context/append", {
        sessionId: input.sessionId,
        entries: input.entries,
      });
      return {
        contextRevision: response.result.contextRevision,
        results: response.result.results.map((result) => {
          if (result.status === "failed") {
            throw new Error(
              `context append failed for ${result.key}: ${result.failure?.message ?? "unknown failure"}`,
            );
          }
          return {
            key: result.key,
            status: result.status,
            ...(result.activationText == null ? {} : { activationText: result.activationText }),
          };
        }),
      };
    },
    async removeChannelContext(input) {
      if (input.keys.length === 0) {
        return { contextRevision: 0, results: [] };
      }
      const client = clientForUniverse(config, input.universeId);
      const response = await client.call("session/context/remove", {
        sessionId: input.sessionId,
        keys: input.keys,
      });
      return {
        contextRevision: response.result.contextRevision,
        results: response.result.results.map((result) => {
          if (result.status === "failed") {
            throw new Error(
              `context remove failed for ${result.key}: ${result.failure?.message ?? "unknown failure"}`,
            );
          }
          return { key: result.key, status: result.status };
        }),
      };
    },
    async reconcileTerminalRun(input) {
      if (input.status === "failed") {
        return { action: "deliver", text: "I couldn't complete that request." };
      }
      if (input.status === "cancelled") {
        return { action: "deliver", text: "That request was cancelled." };
      }
      const client = clientForUniverse(config, input.universeId);
      const response = await client.call("session/read", { sessionId: input.sessionId });
      // Terminal workflow emissions use the engine's numeric run id, while
      // SessionView exposes the public API id (`run_<number>`).
      const runId = `run_${input.runId}`;
      if (runUsedMessagingTool(response.result.session, runId)) {
        return { action: "suppress", reason: "messaging_tool" };
      }
      return {
        action: "deliver",
        text:
          extractAssistantText(response.result.session, runId) ??
          "Lightspeed completed the run, but no assistant text was available.",
      };
    },
  };
}

export function runUsedMessagingTool(session: SessionView, runId: string): boolean {
  const run = session.runs?.find((candidate) => candidate.id === runId);
  if (run?.entries === undefined) {
    return false;
  }
  const messagingCalls = new Set<string>();
  for (const entry of run.entries) {
    if (entry.kind.type === "toolCall" && entry.kind.name.startsWith("message_")) {
      messagingCalls.add(entry.kind.callId);
    }
  }
  return run.entries.some(
    (entry) =>
      entry.kind.type === "toolResult" &&
      !entry.kind.isError &&
      messagingCalls.has(entry.kind.callId),
  );
}

export function extractAssistantText(session: SessionView, runId: string): string | null {
  const run = session.runs?.find((candidate) => candidate.id === runId);
  const texts = assistantTexts(run?.entries);
  return texts.length === 0 ? null : texts.join("\n\n");
}

function assistantTexts(entries: readonly ContextEntryView[] | undefined): string[] {
  return (entries ?? []).flatMap((entry) => {
    if (entry.kind.type !== "message" || entry.kind.role !== "assistant") {
      return [];
    }
    const text = entry.text?.trim();
    return text ? [text] : [];
  });
}

type RpcClient = Pick<LightspeedClient, "call">;

async function putToolAssets(client: RpcClient): Promise<{
  schemas: ChannelToolSchemaRefs;
  descriptions: ChannelToolDescriptionRefs;
}> {
  const schemaEntries = Object.entries(CHANNEL_TOOL_SCHEMAS) as [
    ChannelToolSchemaName,
    (typeof CHANNEL_TOOL_SCHEMAS)[ChannelToolSchemaName],
  ][];
  const descriptionEntries = Object.entries(CHANNEL_TOOL_DESCRIPTIONS) as [
    ChannelToolDescriptionName,
    (typeof CHANNEL_TOOL_DESCRIPTIONS)[ChannelToolDescriptionName],
  ][];
  const response = await client.call("blobs/put", {
    blobs: [
      ...schemaEntries.map(([, schema]) => ({
        bytesBase64: Buffer.from(JSON.stringify(schema), "utf8").toString("base64"),
      })),
      ...descriptionEntries.map(([, description]) => ({
        bytesBase64: Buffer.from(description, "utf8").toString("base64"),
      })),
    ],
  });
  const blobs = response.result.blobs ?? [];
  const expectedCount = schemaEntries.length + descriptionEntries.length;
  if (blobs.length !== expectedCount) {
    throw new Error(`blobs/put returned ${blobs.length} tool assets, expected ${expectedCount}`);
  }
  const schemas = Object.fromEntries(
    schemaEntries.map(([name], index) => {
      const blob = blobs[index];
      if (!blob) {
        throw new Error(`blobs/put omitted schema ${name}`);
      }
      return [name, blob.blobRef];
    }),
  ) as ChannelToolSchemaRefs;
  const descriptions = Object.fromEntries(
    descriptionEntries.map(([name], index) => {
      const blob = blobs[schemaEntries.length + index];
      if (!blob) {
        throw new Error(`blobs/put omitted description ${name}`);
      }
      return [name, blob.blobRef];
    }),
  ) as ChannelToolDescriptionRefs;
  return { schemas, descriptions };
}

function clientForUniverse(config: LightspeedActivityConfig, universeId: string): LightspeedClient {
  return new LightspeedClient({
    endpoint: config.endpoint,
    ...(config.fetch === undefined ? {} : { fetch: config.fetch }),
    headers: { "x-lightspeed-universe": universeId },
  });
}
