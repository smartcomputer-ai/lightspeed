/// GENERATED — do not edit by hand.
/// Source: crates/api/contract/api.schema.json (SessionConfig).
/// Regenerate with: node platform/scripts/generate-config-reference.mjs

export const PROFILE_CONFIG_REFERENCE = `// Every field is optional — omit anything to keep engine defaults.
// Union values are written a | b — pick one.
{
  "context": {
    "compaction": // one of:
      { "mode": "disabled" } |
      { "compactThresholdTokens": 0, "mode": "providerTriggered" } |
      { "compactThresholdTokens": 0, "mode": "providerStandalone", "targetTokens": 0 },
  },
  // Capability grants. An absent feature is not granted; \`{}\` grants it with defaults. Every block carries a behavior \`version\` that pins semantics.
  "features": {
    // Grants active session environments and their process tool surface. Model-driven selection and durable jobs are independent, default-off sub-grants.
    "environments": {
      // Grants the advanced durable-job tool surface. The workflow binding is installed for the session when granted; invocations still require an active, ready environment with matching job capabilities.
      "jobs": true | false,
      // Absent means every registered provider is allowed.
      "providers": ["string"],
      // Registration keys whose registered environments the session may list and activate; absent means every key. Independent of \`providers\`: each list scopes its own environment source, and external environments pass only when neither list is set.
      "registrationKeys": ["string"],
      // Exposes \`environment_list\`, \`environment_activate\`, and \`environment_deactivate\` to the model. \`environment_read\` is available whenever environments are enabled, and external API/profile activation remains available when this is false.
      "selectionTools": true | false,
      // Independent environment skill discovery. Absent disables discovery.
      "skills": {
        // Additional absolute or working-directory-relative discovery roots.
        "additionalRoots": ["string"],
        // Absolute ancestor boundary. Absent scans only the working directory.
        "projectRoot": "string",
        // Absolute session working directory; absent uses the endpoint default.
        "workingDirectory": "string",
      },
      "version": 0,
    },
    // Grants remote MCP tools by declaring linked servers from the universe MCP catalog; must link at least one server, with unique server ids.
    "mcp": {
      "servers": [{
        // (required when this object is present)
        "serverId": "string",
      }],
      "version": 0,
    },
    // Grants sub-agent delegation: \`agent_run\` (joined, result inline) and \`agent_spawn\` (promise, joined with \`await\`) over the listed agent profiles. Limits are root-scoped and attenuating: every descendant of a root session counts against the root, and a nested grant can narrow but never widen the limits pinned on its origin.
    "subagents": {
      // The agent menu. Every id must name an existing profile; the model picks by id and reads descriptions from the sub-agent catalog.
      // (required when this object is present)
      "agents": [{
        // (required when this object is present)
        "profileId": "string",
      }],
      // Per-child run deadline in milliseconds; at most the execution ceiling of 24 hours.
      "deadlineMs": 0,
      // Open sessions under the root at any time, excluding the root.
      "maxConcurrent": 0,
      // A child at depth \`d\` may spawn only while \`d + 1 <= maxDepth\`.
      "maxDepth": 0,
      // Lifetime total of sessions ever created under the root.
      "maxDescendants": 0,
      "version": 0,
    },
    // Grants timer promises through the sleep tool plus the base concurrency tools (await/cancel/detach).
    "timers": {
      "version": 0,
    },
    // Grants the session virtual filesystem. Workspace links declare the session-visible namespace and the VFS catalog is surfaced. Sub-grants are independent; \`{}\` grants a VFS with no tools and no sourcing.
    "vfs": {
      // Prompt-instruction sourcing from the VFS. Absent disables loading; an empty block discovers conventional linked roots.
      "prompts": {
        // Absent searches .agents/prompts and .lightspeed/prompts beneath each workspace link. Explicit roots replace these defaults and must be non-empty absolute paths contained in workspace links.
        "roots": ["string"],
      },
      // Independent VFS skill discovery. Absent disables discovery and removes its runtime catalog; an empty block discovers conventional linked roots.
      "skills": {
        // Absent searches .agents/skills and .lightspeed/skills beneath each workspace link. Explicit roots replace these defaults and must be non-empty absolute paths contained in workspace links.
        "roots": ["string"],
      },
      // Agent-facing filesystem tool surface; absent = no fs tools. Per-path writability is defined by each workspace link's own access. With the environments feature granted, \`readOnly\` also exposes \`vfs_materialize\`; \`edit\` additionally exposes \`vfs_capture\`. Prompt/skill sourcing alone does not grant transfer tools.
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
    // Provider processing class. In session/profile config this becomes the default for every run; in run config it overrides that run. Currently supported only by the built-in OpenAI provider.
    "processingTier": "standard" | "fast" | "flex",
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
