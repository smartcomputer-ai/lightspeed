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
    // Grants session environments. The \`environments\` list is the allowed set: the session can select, read, and run work only on a listed machine, each with its own access grant and working directory. The installed tool surface is the union of every attachment's grant; a call the active machine's grant does not cover fails at execution, so switching machines never changes the toolset. \`{}\` grants the feature with no reachable machine.
    "environments": {
      // The environments this session may use; unique ids, at most one default, at most one \`inherit\` (profiles only).
      "environments": [{
        // Per-attachment environment access, an ordered ladder: \`edit\` adds file editing to \`read\`, \`exec\` adds processes, \`jobs\` adds durable jobs. Processes can write files regardless of the file-tool level, so read-only files with commands is deliberately not expressible.
        // (required when this object is present)
        "access": "read" | "edit" | "exec" | "jobs",
        // Activated when a profile is applied while the session has no active environment; creation is the trivial case. Never overrides a live selection and never applies on a plain \`session/config/put\`.
        "default": true | false,
        "environmentId": "string",
        "inherit": true | false,
        // Absolute machine working directory for file tools, commands, jobs, and sources; absent uses the machine's advertised default.
        "workingDirectory": "string",
      }],
      // Independent environment prompt loading; absent disables sourced instructions.
      "prompts": {
        // Optional source directories, absolute or relative to the environment working directory. Explicit nonempty lists replace all defaults, including home roots. Defaults are .agents/prompts and .lightspeed/prompts under working directory and execution home.
        "roots": ["string"],
      },
      // Exposes \`environment_list\`, \`environment_activate\`, and \`environment_deactivate\` over the attached environments. \`environment_read\` is available whenever environments are enabled, and external API/profile activation remains available when this is false.
      "selection": true | false,
      // Independent environment skill discovery. Absent disables discovery.
      "skills": {
        // Optional source directories, absolute or relative to the environment working directory. Explicit nonempty lists replace all defaults, including home roots. Defaults are .agents/skills and .lightspeed/skills under working directory and execution home.
        "roots": ["string"],
      },
      "version": 0,
    },
    // Grants remote MCP tools by declaring linked servers from the universe MCP catalog; must link at least one server, with unique server ids.
    "mcp": {
      "servers": [{
        // (required when this object is present)
        "serverId": "string",
        // Non-empty subset of the record's allowed tools exposed to this session, under both injection and search; absent exposes the record's full allowlist.
        "tools": ["string"],
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
    // Grants the session virtual filesystem. Workspace attachments declare the session-visible namespace and the VFS catalog is surfaced. The file tool surface is derived from the attachments: any attachment installs the read tools, any \`edit\` attachment adds the write tools, and with the environments feature granted the matching transfer tools appear. \`{}\` grants a VFS with no attachments, no tools, and no sourcing.
    "vfs": {
      // Prompt-instruction sourcing from the VFS. Absent disables loading; an empty block discovers conventional attached roots.
      "prompts": {
        // Absent searches .agents/prompts and .lightspeed/prompts beneath each workspace attachment. Explicit roots replace these defaults and must be non-empty absolute paths contained in workspace attachments.
        "roots": ["string"],
      },
      // Independent VFS skill discovery. Absent disables discovery and removes its runtime catalog; an empty block discovers conventional attached roots.
      "skills": {
        // Absent searches .agents/skills and .lightspeed/skills beneath each workspace attachment. Explicit roots replace these defaults and must be non-empty absolute paths contained in workspace attachments.
        "roots": ["string"],
      },
      "version": 0,
      // Absolute VFS tool working directory; absent uses /.
      "workingDirectory": "string",
      // Catalog resources exposed in the session's workspace namespace at disjoint absolute paths.
      "workspaces": [{
        // Per-attachment VFS access; \`edit\` implies \`read\`.
        // (required when this object is present)
        "access": "read" | "edit",
        // (required when this object is present)
        "path": "string",
        "snapshotRef": "string",
        "workspaceId": "string",
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
