// Canonical named-helper inventory shared by the TypeScript and Python SDKs.
// Each tuple is [operation ID, helper name, ...required positional target fields].
// The remaining parameters form the typed options object (Python keyword args).
// Keep existing signatures stable; add new operations here and document helpers.
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


  ["session/retention/put", "putSessionRetention", "sessionId"],
  ["session/context/remove", "removeContext", "sessionId"],
  ["session/context/replace", "replaceContext", "sessionId"],

  ["environments/credentials/bind", "bindEnvironmentCredential", "environmentId", "envName"],
  ["environments/credentials/list", "listEnvironmentCredentials", "environmentId"],
  ["environments/credentials/unbind", "unbindEnvironmentCredential", "environmentId", "envName"],
  ["environments/update", "updateEnvironment", "environmentId"],
  ["environments/external/create", "createExternalEnvironment"],
  ["environments/registered/put", "putRegisteredEnvironment", "environmentId"],
  ["environments/registered/release", "releaseRegisteredEnvironment", "environmentId"],
  ["environments/ingress/put", "putEnvironmentIngress", "environmentId"],
  ["environments/power/put", "putEnvironmentPower", "environmentId"],
  ["environments/providers/list", "listAvailableEnvironmentProviders"],
  ["environments/providers/read", "readAvailableEnvironmentProvider", "providerId"],
  ["environments/templates/read", "readEnvironmentTemplate", "providerId", "templateId"],
  ["environments/jobs/create", "createEnvironmentJobs", "environmentId"],
  ["environments/jobs/read", "readEnvironmentJobs"],
  ["environments/jobs/cancel", "cancelEnvironmentJobs"],
  ["environments/registration-keys/create", "createEnvironmentRegistrationKey"],
  ["environments/registration-keys/put", "putEnvironmentRegistrationKey", "registrationKeyId"],
  ["environments/registration-keys/read", "readEnvironmentRegistrationKey", "registrationKeyId"],
  ["environments/registration-keys/list", "listEnvironmentRegistrationKeys"],
  ["environments/registration-keys/update", "updateEnvironmentRegistrationKey", "registrationKeyId"],
  ["environments/registration-keys/rotate", "rotateEnvironmentRegistrationKey", "registrationKeyId"],
  ["environments/registration-keys/revoke", "revokeEnvironmentRegistrationKey", "registrationKeyId"],

  ["transcriptions/start", "startTranscription"],
  ["transcriptions/read", "readTranscription", "transcriptionId"],
  ["transcriptions/cancel", "cancelTranscription", "transcriptionId"],
  ["models/defaults/put", "putModelDefaults"],
  ["blobs/put", "putBlobs"],
  ["blobs/has", "hasBlobs"],
  ["vfs/snapshots/commit", "commitSnapshot"],
  ["vfs/snapshots/read", "readSnapshot", "snapshotRef"],
  ["vfs/workspaces/update", "updateWorkspace", "workspaceId"],

  ["mcp/servers/put", "putMcpServer"],
  ["mcp/servers/auth/discover", "discoverMcpServerAuth"],
  ["mcp/servers/tools/discover", "discoverMcpServerTools", "serverId"],
  ["mcp/servers/delete", "deleteMcpServer", "serverId"],

  ["credentials/grants/import", "importCredentialGrant"],
  ["credentials/grants/lease", "leaseCredentialGrant", "grantId"],
  ["credentials/grants/read", "readCredentialGrant", "grantId"],
  ["credentials/grants/list", "listCredentialGrants"],
  ["credentials/grants/put", "putCredentialGrant", "grantId"],
  ["credentials/grants/revoke", "revokeCredentialGrant", "grantId"],
  ["credentials/clients/create", "createCredentialClient"],
  ["credentials/clients/read", "readCredentialClient", "clientId"],
  ["credentials/clients/list", "listCredentialClients"],
  ["credentials/clients/delete", "deleteCredentialClient", "clientId"],
  ["credentials/flows/start", "startCredentialFlow", "clientId"],
  ["credentials/flows/read", "readCredentialFlow", "flowId"],
  ["credentials/providers/create", "createCredentialProvider"],
  ["credentials/providers/put", "putCredentialProvider", "providerId"],
  ["credentials/providers/read", "readCredentialProvider", "providerId"],
  ["credentials/providers/list", "listCredentialProviders"],
  ["credentials/providers/delete", "deleteCredentialProvider", "providerId"],
  ["credentials/github/installations/list", "listGithubInstallations", "providerId"],
  ["credentials/github/installations/grant", "grantGithubInstallation", "providerId"],

  ["bots/create", "createBot"],
  ["bots/put", "putBot"],
  ["bots/close", "closeBot", "botId"],
  ["bots/delete", "deleteBot", "botId"],
  ["bots/state/read", "readBotState", "botId"],
  ["bots/sessions/rotate", "rotateBotSession", "botId", "sessionId"],
  ["bots/triggers/put", "putBotTrigger", "botId"],
  ["bots/triggers/read", "readBotTrigger", "botId", "triggerId"],
  ["bots/triggers/list", "listBotTriggers", "botId"],
  ["bots/triggers/secrets/read", "readBotTriggerSecrets", "botId", "triggerId"],
  ["bots/triggers/delete", "deleteBotTrigger", "botId", "triggerId"],
  ["bots/events/replay", "replayBotEvent", "botId"],
  ["bots/events/read", "readBotEvent", "botId"],
  ["bots/filters/test", "testBotFilter", "botId"],

  ["channels/accounts/create", "createChannelAccount"],
  ["channels/accounts/put", "putChannelAccount"],
  ["channels/accounts/read", "readChannelAccount", "accountId"],
  ["channels/accounts/list", "listChannelAccounts"],
  ["channels/accounts/delete", "deleteChannelAccount", "accountId"],
  ["channels/inbound/admit", "admitChannelInbound", "accountId"],
  ["channels/pairings/list", "listChannelPairings"],
  ["channels/pairings/delete", "deleteChannelPairing", "accountId", "chatId"],
  ["channels/conversations/read", "readChannelConversation", "accountId", "chatId"],

  ["deployment/universes/deleted/list", "listDeletedUniverses"],
  ["deployment/sessions/deleted/list", "listDeletedSessions", "universe"],
  ["deployment/sessions/purge", "purgeSession", "universe", "sessionId"],
  ["deployment/environment-provider-bindings/list", "listDeploymentEnvironmentProviderBindings", "universe"],
  ["deployment/universes/create", "createUniverse", "universeId"],
  ["deployment/universes/list", "listUniverses"],
  ["deployment/universes/read", "readUniverse", "universe"],
  ["deployment/universes/configuration/put", "putUniverseConfiguration", "universe"],
  ["deployment/universes/expiry/put", "putUniverseExpiry", "universe"],
  ["deployment/universes/handle/put", "putUniverseHandle", "universe"],
  ["deployment/universes/delete", "deleteUniverse", "universe"],
  ["deployment/environment-providers/put", "putEnvironmentProvider", "providerId"],
  ["deployment/environment-providers/list", "listEnvironmentProviders"],
  ["deployment/environment-providers/read", "readEnvironmentProvider", "providerId"],
  ["deployment/environment-providers/delete", "deleteEnvironmentProvider", "providerId"],
  ["deployment/environment-providers/bindings/put", "putUniverseEnvironmentProviderBinding", "universe", "providerId"],
  ["deployment/environment-providers/bindings/delete", "deleteUniverseEnvironmentProviderBinding", "universe", "providerId"],
  ["deployment/channels/accounts/list", "listDeploymentChannelAccounts"],
];

// These helpers preserve ergonomic inputs or implement byte/stream transports.
// Both SDK generators validate the actual handwritten implementation exists.
export const MANUAL_HELPERS = [
  ["universe/watch", "watch"],
  ["session/runs/start", "startRun", "sessionId"],
  ["session/runs/steer", "steerRun", "sessionId", "runId"],
  ["session/events/read", "readEvents", "sessionId"],
  ["blobs/read", "readBlobBytes", "blobRef"],
  ["blobs/upload", "uploadBlob", "blobRef"],
];
export const ALL_HELPERS = [...HELPERS, ...MANUAL_HELPERS];

/** Every public runtime operation must have exactly one canonical named helper. */
export function validateHelpers(operations) {
  const expected = new Map(operations.map(operation => [operation.operationId, operation]));
  const seen = new Set();
  const names = new Set();
  for (const [operation, name] of ALL_HELPERS) {
    if (!expected.has(operation)) throw new Error(`Helper ${name} refers to unknown operation ${operation}`);
    if (seen.has(operation)) throw new Error(`Duplicate helper coverage for ${operation}`);
    if (names.has(name)) throw new Error(`Duplicate helper name ${name}`);
    if (!/^[a-z][A-Za-z0-9]*$/.test(name)) throw new Error(`Invalid helper name ${name}`);
    seen.add(operation); names.add(name);
  }
  const missing = [...expected.keys()].filter(operation => !seen.has(operation));
  if (missing.length) throw new Error(`Missing named helpers: ${missing.join(", ")}`);
  for (const [operation, name] of HELPERS) {
    if (["sse", "binaryUpload"].includes(expected.get(operation).transfer)) throw new Error(`Helper ${name} needs a handwritten stream/body implementation`);
  }
}
