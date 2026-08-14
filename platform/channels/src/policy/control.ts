import type { GroupActivation } from "./activation.js";

export type ChannelControlCommand =
  | { kind: "activation"; mode: GroupActivation }
  | { kind: "activation_help" }
  | { kind: "status" };

export function parseChannelControlCommand(text: string): ChannelControlCommand | null {
  const trimmed = text.trim();
  const activation = /^\/activation(?:@[\w_]+)?(?:\s+(\w+))?$/i.exec(trimmed);
  if (activation !== null) {
    const mode = (activation[1] ?? "").toLowerCase();
    return mode === "mention" || mode === "always" || mode === "silent"
      ? { kind: "activation", mode }
      : { kind: "activation_help" };
  }
  return /^\/status(?:@[\w_]+)?$/i.test(trimmed) ? { kind: "status" } : null;
}
