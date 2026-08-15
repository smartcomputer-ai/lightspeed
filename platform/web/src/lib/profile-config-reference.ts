/// GENERATED — do not edit by hand.
/// Source: crates/api/contract/api.schema.json (SessionConfig).
/// Regenerate with: node platform/scripts/generate-config-reference.mjs

export const PROFILE_CONFIG_REFERENCE = `// Every field is optional — omit anything to keep engine defaults.
// Union values are written a | b — pick one.
{
  "context": {
    "compaction": // one of:
      { "mode": "disabled" } |
      { "compact_threshold_tokens": 0, "mode": "providerTriggered" } |
      { "compact_threshold_tokens": 0, "mode": "providerStandalone", "target_tokens": 0 },
  },
  // Capability grants. An absent feature is not granted; \`{}\` grants it with defaults. Every block carries a behavior \`version\` that pins semantics.
  "features": {
    // Grants active session environments and their process tool surface. Model-driven selection and durable jobs are independent, default-off sub-grants.
    "environments": {
      // Grants the advanced durable-job tool surface. The workflow binding is installed for the session when granted; invocations still require an active, ready environment with matching job capabilities.
      "jobs": true | false,
      // Absent means every registered provider is allowed.
      "providers": ["string"],
      // Exposes \`environment_list\`, \`environment_activate\`, and \`environment_deactivate\` to the model. \`environment_read\` is available whenever environments are enabled, and external API/profile activation remains available when this is false.
      "selectionTools": true | false,
      "version": 0,
    },
    // Grants the Fleet subagent control plane (agent_spawn/send/read/list/cancel and profile_list/read).
    "fleet": {
      "profiles": {
        // Absent means all named profiles are visible/readable/spawnable.
        "allow": ["string"],
        "deny": ["string"],
        // Defaults to true when omitted.
        "inline": true | false,
      },
      "spawn": {
        // Absent means all bases are allowed.
        "bases": ["self" | "session" | "profile"],
      },
      "version": 0,
    },
    // Grants remote MCP tools by declaring linked servers from the universe MCP catalog; must link at least one server, with unique server ids.
    "mcp": {
      "servers": [{
        "allowedTools": ["string"],
        "approval": "providerDefault" | "always" | "never",
        "deferLoading": true | false,
        // (required when this object is present)
        "serverId": "string",
      }],
      "version": 0,
    },
    // Grants timer promises through the sleep tool plus the base concurrency tools (await/cancel/detach).
    "timers": {
      "version": 0,
    },
    // Grants the session virtual filesystem. Workspace links declare the session-visible namespace and the VFS catalog is surfaced. Sub-grants are independent; \`{}\` grants a VFS with no tools and no sourcing.
    "vfs": {
      // Prompt-instruction sourcing from the VFS.
      "prompts": {
        // Absent means the conventional roots; an explicit list must be non-empty.
        "roots": ["string"],
      },
      // Skill discovery sourcing from the VFS.
      "skills": {
        // Absent means the conventional roots; an explicit list must be non-empty.
        "roots": ["string"],
      },
      // Agent-facing filesystem tool surface; absent = no fs tools. Per-path writability is defined by each workspace link's own access.
      "tools": "readOnly" | "edit",
      "version": 0,
      // Catalog resources exposed in the session's workspace namespace.
      "workspaceLinks": [{
        // (required when this object is present)
        "access": "readOnly" | "readWrite",
        // (required when this object is present)
        "path": "string",
        // (required when this object is present)
        "target": // one of:
          { "type": "workspace", "workspaceId": "string" } |
          { "snapshotRef": "string", "type": "snapshot" },
      }],
    },
    // Grants network access through the web toolset; \`fetch\` and \`search\` are independently granted, and a web block granting neither is rejected.
    "web": {
      "fetch": {
      },
      "search": {
        // Absent means all domains; an explicit list must be non-empty.
        "allowedDomains": ["string"],
        "blockedDomains": ["string"],
      },
      "version": 0,
    },
  },
  // Turn-shaping defaults applied to every LLM generation. Per-run overrides ride \`session/runs/start\`.
  "generation": {
    "maxOutputTokens": 0,
    // Whether the model may call several tools in one turn; absent leaves the provider default.
    "parallelToolUse": true | false,
    // Reasoning effort tier as a provider-native string (e.g. "none", "high", "xhigh", "max"); validated against the session's provider.
    "reasoningEffort": "string",
    "toolChoice": // one of:
      { "type": "auto" } |
      { "type": "none" } |
      { "type": "requiredAny" } |
      { "toolId": "string", "type": "specific" },
  },
  // Run budget defaults enforced by the engine drive loop.
  "limits": {
    "maxToolRounds": 0,
    "maxTurns": 0,
  },
  // Absent on input means the deployment default model. Documents read back from a session always carry the model; the provider api kind is pinned for the session's lifetime.
  "model": {
    // (required when this object is present)
    "apiKind": "string",
    // (required when this object is present)
    "model": "string",
    // (required when this object is present)
    "providerId": "string",
  },
}
`;
