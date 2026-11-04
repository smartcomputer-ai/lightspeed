// The methods `LightspeedClient` names as helpers. Everything in the API stays
// reachable through `client.call`; a helper is for what most applications need to
// use Lightspeed: sessions, runs, and the resources sessions attach. Administration
// (auth, deployment, channels, registration keys) stays on `call`.
//
// Each entry is [method, helper name, ...IDs naming the target]. Those IDs become
// positional arguments and the method's other params become the options object.
// A helper needing behavior of its own, such as `startRun`, is written by hand in
// src/client.ts instead. Add new helpers to the table in README.md too.
export const HELPERS = [
  ["initialize", "initialize"],

  ["session/start", "startSession"],
  ["session/read", "readSession", "sessionId"],
  ["session/list", "listSessions"],
  ["session/rename", "renameSession", "sessionId"],
  ["session/metadata/put", "putSessionMetadata", "sessionId"],
  ["session/config/put", "putSessionConfig", "sessionId"],
  ["session/share", "shareSession", "sessionId"],
  ["session/restart", "restartSession", "sessionId"],
  ["session/close", "closeSession", "sessionId"],
  ["session/delete", "deleteSession", "sessionId"],

  ["session/runs/list", "listRuns", "sessionId"],
  ["session/runs/read", "readRun", "sessionId", "runId"],
  ["session/runs/cancel", "cancelRun", "sessionId", "runId"],
  ["session/runs/approvals/decide", "decideApprovals", "sessionId", "runId"],

  ["session/context/append", "appendContext", "sessionId"],
  ["session/context/compact", "compactContext", "sessionId"],
  ["session/profiles/apply", "applyProfile", "sessionId"],
  ["session/skills/list", "listSessionSkills", "sessionId"],
  ["session/environments/activate", "activateEnvironment", "sessionId", "environmentId"],
  ["session/environments/deactivate", "deactivateEnvironment", "sessionId"],

  ["profiles/create", "createProfile"],
  ["profiles/read", "readProfile", "profileId"],
  ["profiles/list", "listProfiles"],
  ["profiles/put", "putProfile"],
  ["profiles/delete", "deleteProfile", "profileId"],

  ["models/list", "listModels"],
  ["models/defaults/read", "readModelDefaults"],

  ["environments/create", "createEnvironment"],
  ["environments/read", "readEnvironment", "environmentId"],
  ["environments/list", "listEnvironments"],
  ["environments/close", "closeEnvironment", "environmentId"],
  ["environments/templates/list", "listEnvironmentTemplates"],

  ["vfs/workspaces/create", "createWorkspace"],
  ["vfs/workspaces/read", "readWorkspace", "workspaceId"],
  ["vfs/workspaces/list", "listWorkspaces"],
  ["vfs/workspaces/delete", "deleteWorkspace", "workspaceId"],
  ["vfs/workspaces/files/read", "readWorkspaceFile", "workspaceId", "path"],

  ["mcp/servers/read", "readMcpServer", "serverId"],
  ["mcp/servers/list", "listMcpServers"],

  ["bots/read", "readBot", "botId"],
  ["bots/list", "listBots"],
  ["bots/events/list", "listBotEvents", "botId"],
  ["bots/events/admit", "admitBotEvent", "botId"],
];
