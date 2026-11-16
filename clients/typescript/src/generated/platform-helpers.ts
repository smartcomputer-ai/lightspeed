/* Generated from platform/shared/contract by scripts/generate-platform.mjs. Do not edit. */
import type { CallOptions } from "../client.js";
import type { PlatformOperationId, PlatformOperationParams, PlatformOperationResult } from "./platform.js";
interface PlatformCaller { call<K extends PlatformOperationId>(operation: K, params: PlatformOperationParams<K>, options?: CallOptions): Promise<PlatformOperationResult<K>>; }
export const PLATFORM_CLIENT_HELPERS = {
  "readCurrentSession": "platform/session/read",
  "revokeCurrentSession": "platform/session/revoke",
  "listPersonalApiKeys": "platform/credentials/list",
  "createPersonalApiKey": "platform/credentials/create",
  "revokePersonalApiKey": "platform/credentials/revoke",
  "listApplicationApiKeys": "platform/applications/credentials/list",
  "createApplicationApiKey": "platform/applications/credentials/create",
  "revokeApplicationApiKey": "platform/applications/credentials/revoke",
  "listAccounts": "platform/accounts/list",
  "createAccount": "platform/accounts/create",
  "readAccount": "platform/accounts/read",
  "updateAccount": "platform/accounts/update",
  "listAccountMembers": "platform/accounts/members/list",
  "putAccountMember": "platform/accounts/members/put",
  "removeAccountMember": "platform/accounts/members/delete",
  "listApplications": "platform/accounts/applications/list",
  "createApplication": "platform/accounts/applications/create",
  "updateApplication": "platform/accounts/applications/update",
  "disableApplication": "platform/accounts/applications/delete",
  "adoptUniverse": "platform/universes/adopt",
  "createUniverse": "platform/universes/create",
  "updateUniverse": "platform/universes/update",
  "listUniverseGrants": "platform/universes/grants/list",
  "putUniverseGrant": "platform/universes/grants/put",
  "removeUniverseGrant": "platform/universes/grants/delete",
  "putUniverseAccessPolicy": "platform/universes/access-policy/put"
} as const;
export class PlatformHelpers {
  constructor(private readonly client: PlatformCaller) {}
  readCurrentSession(params: PlatformOperationParams<"platform/session/read"> = {}, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/session/read">> {
    return this.client.call("platform/session/read", params, options);
  }
  revokeCurrentSession(params: PlatformOperationParams<"platform/session/revoke"> = {}, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/session/revoke">> {
    return this.client.call("platform/session/revoke", params, options);
  }
  listPersonalApiKeys(params: PlatformOperationParams<"platform/credentials/list"> = {}, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/credentials/list">> {
    return this.client.call("platform/credentials/list", params, options);
  }
  createPersonalApiKey(params: PlatformOperationParams<"platform/credentials/create">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/credentials/create">> {
    return this.client.call("platform/credentials/create", params, options);
  }
  revokePersonalApiKey(params: PlatformOperationParams<"platform/credentials/revoke">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/credentials/revoke">> {
    return this.client.call("platform/credentials/revoke", params, options);
  }
  listApplicationApiKeys(params: PlatformOperationParams<"platform/applications/credentials/list">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/applications/credentials/list">> {
    return this.client.call("platform/applications/credentials/list", params, options);
  }
  createApplicationApiKey(params: PlatformOperationParams<"platform/applications/credentials/create">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/applications/credentials/create">> {
    return this.client.call("platform/applications/credentials/create", params, options);
  }
  revokeApplicationApiKey(params: PlatformOperationParams<"platform/applications/credentials/revoke">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/applications/credentials/revoke">> {
    return this.client.call("platform/applications/credentials/revoke", params, options);
  }
  listAccounts(params: PlatformOperationParams<"platform/accounts/list"> = {}, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/accounts/list">> {
    return this.client.call("platform/accounts/list", params, options);
  }
  createAccount(params: PlatformOperationParams<"platform/accounts/create">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/accounts/create">> {
    return this.client.call("platform/accounts/create", params, options);
  }
  readAccount(params: PlatformOperationParams<"platform/accounts/read">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/accounts/read">> {
    return this.client.call("platform/accounts/read", params, options);
  }
  updateAccount(params: PlatformOperationParams<"platform/accounts/update">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/accounts/update">> {
    return this.client.call("platform/accounts/update", params, options);
  }
  listAccountMembers(params: PlatformOperationParams<"platform/accounts/members/list">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/accounts/members/list">> {
    return this.client.call("platform/accounts/members/list", params, options);
  }
  putAccountMember(params: PlatformOperationParams<"platform/accounts/members/put">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/accounts/members/put">> {
    return this.client.call("platform/accounts/members/put", params, options);
  }
  removeAccountMember(params: PlatformOperationParams<"platform/accounts/members/delete">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/accounts/members/delete">> {
    return this.client.call("platform/accounts/members/delete", params, options);
  }
  listApplications(params: PlatformOperationParams<"platform/accounts/applications/list">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/accounts/applications/list">> {
    return this.client.call("platform/accounts/applications/list", params, options);
  }
  createApplication(params: PlatformOperationParams<"platform/accounts/applications/create">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/accounts/applications/create">> {
    return this.client.call("platform/accounts/applications/create", params, options);
  }
  updateApplication(params: PlatformOperationParams<"platform/accounts/applications/update">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/accounts/applications/update">> {
    return this.client.call("platform/accounts/applications/update", params, options);
  }
  disableApplication(params: PlatformOperationParams<"platform/accounts/applications/delete">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/accounts/applications/delete">> {
    return this.client.call("platform/accounts/applications/delete", params, options);
  }
  adoptUniverse(params: PlatformOperationParams<"platform/universes/adopt">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/universes/adopt">> {
    return this.client.call("platform/universes/adopt", params, options);
  }
  createUniverse(params: PlatformOperationParams<"platform/universes/create">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/universes/create">> {
    return this.client.call("platform/universes/create", params, options);
  }
  updateUniverse(params: PlatformOperationParams<"platform/universes/update">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/universes/update">> {
    return this.client.call("platform/universes/update", params, options);
  }
  listUniverseGrants(params: PlatformOperationParams<"platform/universes/grants/list">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/universes/grants/list">> {
    return this.client.call("platform/universes/grants/list", params, options);
  }
  putUniverseGrant(params: PlatformOperationParams<"platform/universes/grants/put">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/universes/grants/put">> {
    return this.client.call("platform/universes/grants/put", params, options);
  }
  removeUniverseGrant(params: PlatformOperationParams<"platform/universes/grants/delete">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/universes/grants/delete">> {
    return this.client.call("platform/universes/grants/delete", params, options);
  }
  putUniverseAccessPolicy(params: PlatformOperationParams<"platform/universes/access-policy/put">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/universes/access-policy/put">> {
    return this.client.call("platform/universes/access-policy/put", params, options);
  }
}
