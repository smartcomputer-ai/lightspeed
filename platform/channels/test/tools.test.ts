import { describe, expect, it } from "vitest";
import {
  CHANNEL_TOOL_DEADLINE_MS,
  CHANNEL_TOOL_DESCRIPTIONS,
  CHANNEL_TOOL_SCHEMAS,
  channelWorkflowTools,
  type ChannelToolSchemaRefs,
} from "../src/contracts/tools.js";

const refs: ChannelToolSchemaRefs = {
  sendInput: "blob:send",
  editInput: "blob:edit",
  reactInput: "blob:react",
  noopInput: "blob:noop",
  deliveryReceipt: "blob:receipt",
};
const descriptions = Object.fromEntries(
  Object.keys(CHANNEL_TOOL_DESCRIPTIONS).map((name) => [name, `blob:description-${name}`]),
) as Record<keyof typeof CHANNEL_TOOL_DESCRIPTIONS, string>;

describe("channelWorkflowTools", () => {
  it("makes every strict tool property required", () => {
    for (const schema of [
      CHANNEL_TOOL_SCHEMAS.sendInput,
      CHANNEL_TOOL_SCHEMAS.editInput,
      CHANNEL_TOOL_SCHEMAS.reactInput,
      CHANNEL_TOOL_SCHEMAS.noopInput,
    ]) {
      expect(new Set(schema.required)).toEqual(new Set(Object.keys(schema.properties)));
    }
  });

  it("declares Joined delivery tools and an Accepted no-op", () => {
    const receiver = { workflowId: "channels/one", workflowKind: "channelSessionWorkflowV1" };
    const tools = channelWorkflowTools(receiver, refs, descriptions);

    expect(tools.map((tool) => tool.definition.tool.name)).toEqual([
      "message_send",
      "message_edit",
      "message_react",
      "message_noop",
    ]);
    for (const tool of tools.slice(0, 3)) {
      expect(tool.target).toEqual({ type: "bound", receiver, dispatch: "push" });
      expect(tool.completion).toEqual({
        type: "joined",
        deadlineAfterMs: CHANNEL_TOOL_DEADLINE_MS,
        replySchemaRef: refs.deliveryReceipt,
      });
    }
    expect(tools.map((tool) => tool.definition.tool.kind.descriptionRef)).toEqual([
      descriptions.send,
      descriptions.edit,
      descriptions.react,
      descriptions.noop,
    ]);
    expect(tools[3]?.target).toEqual({ type: "bound", receiver, dispatch: "pull" });
    expect(tools[3]?.completion).toEqual({ type: "accepted" });
  });

  it("does not instruct the model to await Joined tools", () => {
    for (const description of [
      CHANNEL_TOOL_DESCRIPTIONS.send,
      CHANNEL_TOOL_DESCRIPTIONS.edit,
      CHANNEL_TOOL_DESCRIPTIONS.react,
    ]) {
      expect(description).not.toMatch(/promise|\bawait\b/i);
      expect(description).toMatch(/completes after the provider acknowledges/i);
    }
  });
});
