import type { SessionManagement } from "@/api";

export function managedSessionOwnerLabel(
  management: SessionManagement | null | undefined,
): string {
  const kind = management?.lifecycleController?.workflowKind;
  if (kind === "channelSessionWorkflowV1") return "Channels";
  return kind || "an external workflow";
}
