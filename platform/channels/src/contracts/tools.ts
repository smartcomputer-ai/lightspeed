import type {
  WorkflowEndpointInput,
  WorkflowToolDeclarationInput,
} from "@lightspeed/agent-client";

export const CHANNEL_TOOL_DEADLINE_MS = 120_000;

export const CHANNEL_TOOL_DESCRIPTIONS = {
  send:
    "Send a message to the current channel conversation. Use an exact provider message id from an inbound envelope for replyTo, or null. This completes after the provider acknowledges delivery and returns the provider message ids.",
  edit:
    "Edit a message previously sent by this channel account. Use the exact provider message id. This completes after the provider acknowledges the edit.",
  react:
    "React to a channel message. Use the exact provider message id shown after # in the inbound envelope. This completes after the provider acknowledges the reaction.",
  noop: "Deliberately send no reply to the channel.",
} as const;

export const CHANNEL_TOOL_SCHEMAS = {
  sendInput: {
    type: "object",
    description: "Send a message to the current channel conversation.",
    properties: {
      text: { type: "string", minLength: 1, description: "Message text in Markdown." },
      replyTo: {
        type: ["string", "null"],
        description: "Optional provider message id to reply to.",
      },
    },
    // Strict function schemas must require every declared property. Optional
    // values are represented as required nullable properties for providers
    // (such as OpenAI Responses) that enforce that invariant.
    required: ["text", "replyTo"],
    additionalProperties: false,
  },
  editInput: {
    type: "object",
    description: "Edit a message previously sent by this channel account.",
    properties: {
      messageId: { type: "string", minLength: 1 },
      text: { type: "string", minLength: 1, description: "Replacement text in Markdown." },
    },
    required: ["messageId", "text"],
    additionalProperties: false,
  },
  reactInput: {
    type: "object",
    description: "React to a channel message.",
    properties: {
      messageId: { type: "string", minLength: 1 },
      emoji: { type: "string", minLength: 1 },
    },
    required: ["messageId", "emoji"],
    additionalProperties: false,
  },
  noopInput: {
    type: "object",
    description: "Deliberately send no reply to the channel.",
    properties: {
      reason: { type: "string" },
    },
    required: ["reason"],
    additionalProperties: false,
  },
  deliveryReceipt: {
    type: "object",
    properties: {
      provider: { type: "string", enum: ["telegram", "whatsapp"] },
      messageIds: {
        type: "array",
        items: { type: "string", minLength: 1 },
        minItems: 1,
        maxItems: 32,
      },
    },
    required: ["provider", "messageIds"],
    additionalProperties: false,
  },
} as const;

export type ChannelToolSchemaName = keyof typeof CHANNEL_TOOL_SCHEMAS;
export type ChannelToolSchemaRefs = Record<ChannelToolSchemaName, string>;
export type ChannelToolDescriptionName = keyof typeof CHANNEL_TOOL_DESCRIPTIONS;
export type ChannelToolDescriptionRefs = Record<ChannelToolDescriptionName, string>;

export function channelWorkflowTools(
  receiver: WorkflowEndpointInput,
  schemas: ChannelToolSchemaRefs,
  descriptions: ChannelToolDescriptionRefs,
): WorkflowToolDeclarationInput[] {
  const joined = (
    toolId: string,
    semanticType: string,
    name: string,
    inputSchemaRef: string,
    descriptionRef: string,
  ): WorkflowToolDeclarationInput => ({
    definition: {
      toolId,
      revision: 1,
      semanticType,
      tool: {
        name,
        parallelism: "exclusive",
        kind: {
          type: "function",
          inputSchemaRef,
          descriptionRef,
          strict: true,
        },
      },
    },
    target: { type: "bound", receiver, dispatch: "push" },
    completion: {
      type: "joined",
      deadlineAfterMs: CHANNEL_TOOL_DEADLINE_MS,
      replySchemaRef: schemas.deliveryReceipt,
    },
  });

  return [
    joined(
      "channels.message_send.v1",
      "channels.message.send.v1",
      "message_send",
      schemas.sendInput,
      descriptions.send,
    ),
    joined(
      "channels.message_edit.v1",
      "channels.message.edit.v1",
      "message_edit",
      schemas.editInput,
      descriptions.edit,
    ),
    joined(
      "channels.message_react.v1",
      "channels.message.react.v1",
      "message_react",
      schemas.reactInput,
      descriptions.react,
    ),
    {
      definition: {
        toolId: "channels.message_noop.v1",
        revision: 1,
        semanticType: "channels.message.noop.v1",
        tool: {
          name: "message_noop",
          parallelism: "exclusive",
          kind: {
            type: "function",
            inputSchemaRef: schemas.noopInput,
            descriptionRef: descriptions.noop,
            strict: true,
          },
        },
      },
      target: { type: "bound", receiver, dispatch: "pull" },
      completion: { type: "accepted" },
    },
  ];
}
