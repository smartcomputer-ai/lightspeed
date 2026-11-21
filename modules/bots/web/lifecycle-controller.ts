import type { SessionIntegration } from "@lightspeed-ai/platform-web/modules/client";

const BOT_CONTROLLER_KINDS = new Set([
  "BotControllerWorkflow",
  "botControllerWorkflowV1",
  "bot_controller_v1",
]);

export function isBotController(integration: SessionIntegration | null | undefined): boolean {
  const kind = integration?.lifecycleController?.workflowKind;
  return Boolean(kind && BOT_CONTROLLER_KINDS.has(kind));
}

export function controlledSessionOwnerLabel(
  integration: SessionIntegration | null | undefined,
): string {
  const kind = integration?.lifecycleController?.workflowKind;
  if (isBotController(integration)) return "a bot";
  return kind || "an external workflow";
}

/** Resolve the owning bot only when the lifecycle controller is a bot controller. */
export function controlledSessionBotId(
  integration: SessionIntegration | null | undefined,
  metadata: Record<string, string> | undefined,
): string | null {
  const controller = integration?.lifecycleController;
  if (!controller || !isBotController(integration)) return null;

  const metadataId = metadata?.bot?.trim();
  if (metadataId) return metadataId;

  const current = controller.workflowId.match(/(?:^|\/)bot-([^/]+)$/)?.[1];
  if (current) return current;
  const demo = controller.workflowId.match(/^bot:v1:([^:]+)$/)?.[1];
  if (demo) return demo;
  return controller.workflowId.match(/^lightspeed\.bots\.v1\/[^/]+\/([^/]+)$/)?.[1] ?? null;
}
