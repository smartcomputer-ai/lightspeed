import { z } from "zod";

export const AGENT_MODEL_API_KINDS = ["openai:responses", "openai:completions", "anthropic:messages"] as const;
const routeName = z.string().min(1).refine(
  (value) => value.trim() === value && new TextEncoder().encode(value).length <= 512,
  "Use 1–512 bytes without surrounding whitespace.",
);

export const modelDefaultsPutSchema = z.object({
  slot: z.enum(["agentRun", "speechToText"]),
  model: z.object({ providerId: routeName, apiKind: z.string(), model: routeName }).strict().nullable(),
  expectedRevision: z.number().int().min(0).max(Number.MAX_SAFE_INTEGER),
}).strict().refine(({ slot, model }) => !model || (slot === "agentRun"
  ? (AGENT_MODEL_API_KINDS as readonly string[]).includes(model.apiKind)
  : model.apiKind === "openai:audio-transcriptions"), {
  message: "The selected API does not support this use.", path: ["model", "apiKind"],
});
