/* Generated from platform/shared/contract by scripts/generate-platform.mjs. Do not edit. */
import type { CallOptions } from "../client.js";
import type { PlatformOperationId, PlatformOperationParams, PlatformOperationResult } from "./platform.js";
interface PlatformCaller { call<K extends PlatformOperationId>(operation: K, params: PlatformOperationParams<K>, options?: CallOptions): Promise<PlatformOperationResult<K>>; }
export const PLATFORM_CLIENT_HELPERS = {
  "readCurrentSession": "platform/session/read",
  "revokeCurrentSession": "platform/session/revoke",
  "listPersonalApiKeys": "platform/credentials/list",
  "createPersonalApiKey": "platform/credentials/create",
  "replacePersonalApiKey": "platform/credentials/replace",
  "revokePersonalApiKey": "platform/credentials/revoke",
  "listOrganizationApiKeys": "platform/organizations/credentials/list",
  "listServiceUserApiKeys": "platform/service-users/credentials/list",
  "createServiceUserApiKey": "platform/service-users/credentials/create",
  "replaceServiceUserApiKey": "platform/service-users/credentials/replace",
  "putServiceUserApiKey": "platform/service-users/credentials/put",
  "revokeServiceUserApiKey": "platform/service-users/credentials/revoke",
  "listPlatformSettings": "platform/settings/list",
  "putPlatformSetting": "platform/settings/put",
  "listOrganizations": "platform/organizations/list",
  "enrollOrganization": "platform/organizations/enroll",
  "createOrganization": "platform/organizations/create",
  "readOrganization": "platform/organizations/read",
  "updateOrganization": "platform/organizations/update",
  "listOrganizationMembers": "platform/organizations/members/list",
  "putOrganizationMember": "platform/organizations/members/put",
  "removeOrganizationMember": "platform/organizations/members/delete",
  "listServiceUsers": "platform/organizations/service-users/list",
  "createServiceUser": "platform/organizations/service-users/create",
  "updateServiceUser": "platform/organizations/service-users/update",
  "listFederatedCredentials": "platform/service-users/federated-credentials/list",
  "putFederatedCredential": "platform/service-users/federated-credentials/put",
  "deleteFederatedCredential": "platform/service-users/federated-credentials/delete",
  "adoptUniverse": "platform/universes/adopt",
  "createUniverse": "platform/universes/create",
  "updateUniverse": "platform/universes/update",
  "listUniverseGrants": "platform/universes/grants/list",
  "putUniverseGrant": "platform/universes/grants/put",
  "removeUniverseGrant": "platform/universes/grants/delete",
  "readPlatformUniverse": "platform/universes/read",
  "readUniverseAccessPolicy": "platform/universes/access-policy/read",
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
  replacePersonalApiKey(params: PlatformOperationParams<"platform/credentials/replace">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/credentials/replace">> {
    return this.client.call("platform/credentials/replace", params, options);
  }
  revokePersonalApiKey(params: PlatformOperationParams<"platform/credentials/revoke">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/credentials/revoke">> {
    return this.client.call("platform/credentials/revoke", params, options);
  }
  listOrganizationApiKeys(params: PlatformOperationParams<"platform/organizations/credentials/list">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/organizations/credentials/list">> {
    return this.client.call("platform/organizations/credentials/list", params, options);
  }
  listServiceUserApiKeys(params: PlatformOperationParams<"platform/service-users/credentials/list">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/service-users/credentials/list">> {
    return this.client.call("platform/service-users/credentials/list", params, options);
  }
  createServiceUserApiKey(params: PlatformOperationParams<"platform/service-users/credentials/create">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/service-users/credentials/create">> {
    return this.client.call("platform/service-users/credentials/create", params, options);
  }
  replaceServiceUserApiKey(params: PlatformOperationParams<"platform/service-users/credentials/replace">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/service-users/credentials/replace">> {
    return this.client.call("platform/service-users/credentials/replace", params, options);
  }
  putServiceUserApiKey(params: PlatformOperationParams<"platform/service-users/credentials/put">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/service-users/credentials/put">> {
    return this.client.call("platform/service-users/credentials/put", params, options);
  }
  revokeServiceUserApiKey(params: PlatformOperationParams<"platform/service-users/credentials/revoke">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/service-users/credentials/revoke">> {
    return this.client.call("platform/service-users/credentials/revoke", params, options);
  }
  listPlatformSettings(params: PlatformOperationParams<"platform/settings/list"> = {}, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/settings/list">> {
    return this.client.call("platform/settings/list", params, options);
  }
  putPlatformSetting(params: PlatformOperationParams<"platform/settings/put">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/settings/put">> {
    return this.client.call("platform/settings/put", params, options);
  }
  listOrganizations(params: PlatformOperationParams<"platform/organizations/list"> = {}, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/organizations/list">> {
    return this.client.call("platform/organizations/list", params, options);
  }
  enrollOrganization(params: PlatformOperationParams<"platform/organizations/enroll">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/organizations/enroll">> {
    return this.client.call("platform/organizations/enroll", params, options);
  }
  createOrganization(params: PlatformOperationParams<"platform/organizations/create">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/organizations/create">> {
    return this.client.call("platform/organizations/create", params, options);
  }
  readOrganization(params: PlatformOperationParams<"platform/organizations/read">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/organizations/read">> {
    return this.client.call("platform/organizations/read", params, options);
  }
  updateOrganization(params: PlatformOperationParams<"platform/organizations/update">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/organizations/update">> {
    return this.client.call("platform/organizations/update", params, options);
  }
  listOrganizationMembers(params: PlatformOperationParams<"platform/organizations/members/list">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/organizations/members/list">> {
    return this.client.call("platform/organizations/members/list", params, options);
  }
  putOrganizationMember(params: PlatformOperationParams<"platform/organizations/members/put">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/organizations/members/put">> {
    return this.client.call("platform/organizations/members/put", params, options);
  }
  removeOrganizationMember(params: PlatformOperationParams<"platform/organizations/members/delete">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/organizations/members/delete">> {
    return this.client.call("platform/organizations/members/delete", params, options);
  }
  listServiceUsers(params: PlatformOperationParams<"platform/organizations/service-users/list">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/organizations/service-users/list">> {
    return this.client.call("platform/organizations/service-users/list", params, options);
  }
  createServiceUser(params: PlatformOperationParams<"platform/organizations/service-users/create">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/organizations/service-users/create">> {
    return this.client.call("platform/organizations/service-users/create", params, options);
  }
  updateServiceUser(params: PlatformOperationParams<"platform/organizations/service-users/update">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/organizations/service-users/update">> {
    return this.client.call("platform/organizations/service-users/update", params, options);
  }
  listFederatedCredentials(params: PlatformOperationParams<"platform/service-users/federated-credentials/list">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/service-users/federated-credentials/list">> {
    return this.client.call("platform/service-users/federated-credentials/list", params, options);
  }
  putFederatedCredential(params: PlatformOperationParams<"platform/service-users/federated-credentials/put">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/service-users/federated-credentials/put">> {
    return this.client.call("platform/service-users/federated-credentials/put", params, options);
  }
  deleteFederatedCredential(params: PlatformOperationParams<"platform/service-users/federated-credentials/delete">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/service-users/federated-credentials/delete">> {
    return this.client.call("platform/service-users/federated-credentials/delete", params, options);
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
  readPlatformUniverse(params: PlatformOperationParams<"platform/universes/read">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/universes/read">> {
    return this.client.call("platform/universes/read", params, options);
  }
  readUniverseAccessPolicy(params: PlatformOperationParams<"platform/universes/access-policy/read">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/universes/access-policy/read">> {
    return this.client.call("platform/universes/access-policy/read", params, options);
  }
  putUniverseAccessPolicy(params: PlatformOperationParams<"platform/universes/access-policy/put">, options: CallOptions = {}): Promise<PlatformOperationResult<"platform/universes/access-policy/put">> {
    return this.client.call("platform/universes/access-policy/put", params, options);
  }
}
