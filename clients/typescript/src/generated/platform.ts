/* Generated from platform/shared/contract by scripts/generate-platform.mjs. Do not edit. */
import type * as T from "./platform-types.js";
export interface PlatformOperationMap {
  "platform/session/read": { params: T.PlatformSessionReadParams; result: T.PlatformSessionReadResult };
  "platform/session/revoke": { params: T.PlatformSessionRevokeParams; result: T.PlatformSessionRevokeResult };
  "platform/credentials/list": { params: T.PlatformCredentialsListParams; result: T.PlatformCredentialsListResult };
  "platform/credentials/create": { params: T.PlatformCredentialsCreateParams; result: T.PlatformCredentialsCreateResult };
  "platform/credentials/revoke": { params: T.PlatformCredentialsRevokeParams; result: T.PlatformCredentialsRevokeResult };
  "platform/organizations/credentials/list": { params: T.PlatformOrganizationsCredentialsListParams; result: T.PlatformOrganizationsCredentialsListResult };
  "platform/service-users/credentials/list": { params: T.PlatformServiceUsersCredentialsListParams; result: T.PlatformServiceUsersCredentialsListResult };
  "platform/service-users/credentials/create": { params: T.PlatformServiceUsersCredentialsCreateParams; result: T.PlatformServiceUsersCredentialsCreateResult };
  "platform/service-users/credentials/revoke": { params: T.PlatformServiceUsersCredentialsRevokeParams; result: T.PlatformServiceUsersCredentialsRevokeResult };
  "platform/organizations/list": { params: T.PlatformOrganizationsListParams; result: T.PlatformOrganizationsListResult };
  "platform/organizations/create": { params: T.PlatformOrganizationsCreateParams; result: T.PlatformOrganizationsCreateResult };
  "platform/organizations/read": { params: T.PlatformOrganizationsReadParams; result: T.PlatformOrganizationsReadResult };
  "platform/organizations/update": { params: T.PlatformOrganizationsUpdateParams; result: T.PlatformOrganizationsUpdateResult };
  "platform/organizations/members/list": { params: T.PlatformOrganizationsMembersListParams; result: T.PlatformOrganizationsMembersListResult };
  "platform/organizations/members/put": { params: T.PlatformOrganizationsMembersPutParams; result: T.PlatformOrganizationsMembersPutResult };
  "platform/organizations/members/delete": { params: T.PlatformOrganizationsMembersDeleteParams; result: T.PlatformOrganizationsMembersDeleteResult };
  "platform/organizations/service-users/list": { params: T.PlatformOrganizationsServiceUsersListParams; result: T.PlatformOrganizationsServiceUsersListResult };
  "platform/organizations/service-users/create": { params: T.PlatformOrganizationsServiceUsersCreateParams; result: T.PlatformOrganizationsServiceUsersCreateResult };
  "platform/organizations/service-users/update": { params: T.PlatformOrganizationsServiceUsersUpdateParams; result: T.PlatformOrganizationsServiceUsersUpdateResult };
  "platform/organizations/service-users/delete": { params: T.PlatformOrganizationsServiceUsersDeleteParams; result: T.PlatformOrganizationsServiceUsersDeleteResult };
  "platform/universes/adopt": { params: T.PlatformUniversesAdoptParams; result: T.PlatformUniversesAdoptResult };
  "platform/universes/create": { params: T.PlatformUniversesCreateParams; result: T.PlatformUniversesCreateResult };
  "platform/universes/update": { params: T.PlatformUniversesUpdateParams; result: T.PlatformUniversesUpdateResult };
  "platform/universes/grants/list": { params: T.PlatformUniversesGrantsListParams; result: T.PlatformUniversesGrantsListResult };
  "platform/universes/grants/put": { params: T.PlatformUniversesGrantsPutParams; result: T.PlatformUniversesGrantsPutResult };
  "platform/universes/grants/delete": { params: T.PlatformUniversesGrantsDeleteParams; result: T.PlatformUniversesGrantsDeleteResult };
  "platform/universes/access-policy/put": { params: T.PlatformUniversesAccessPolicyPutParams; result: T.PlatformUniversesAccessPolicyPutResult };
}
export type PlatformOperationId = keyof PlatformOperationMap;
export type PlatformOperationParams<K extends PlatformOperationId> = PlatformOperationMap[K]["params"];
export type PlatformOperationResult<K extends PlatformOperationId> = PlatformOperationMap[K]["result"];
export const PLATFORM_OPERATIONS = {
  "platform/session/read": {
    "method": "GET",
    "path": "/api/v1/platform/session"
  },
  "platform/session/revoke": {
    "method": "DELETE",
    "path": "/api/v1/platform/session"
  },
  "platform/credentials/list": {
    "method": "GET",
    "path": "/api/v1/platform/credentials"
  },
  "platform/credentials/create": {
    "method": "POST",
    "path": "/api/v1/platform/credentials"
  },
  "platform/credentials/revoke": {
    "method": "DELETE",
    "path": "/api/v1/platform/credentials/{keyId}"
  },
  "platform/organizations/credentials/list": {
    "method": "GET",
    "path": "/api/v1/platform/organizations/{organizationId}/credentials"
  },
  "platform/service-users/credentials/list": {
    "method": "GET",
    "path": "/api/v1/platform/organizations/{organizationId}/service-users/{serviceUserId}/credentials"
  },
  "platform/service-users/credentials/create": {
    "method": "POST",
    "path": "/api/v1/platform/organizations/{organizationId}/service-users/{serviceUserId}/credentials"
  },
  "platform/service-users/credentials/revoke": {
    "method": "DELETE",
    "path": "/api/v1/platform/organizations/{organizationId}/service-users/{serviceUserId}/credentials/{keyId}"
  },
  "platform/organizations/list": {
    "method": "GET",
    "path": "/api/v1/platform/organizations"
  },
  "platform/organizations/create": {
    "method": "POST",
    "path": "/api/v1/platform/organizations"
  },
  "platform/organizations/read": {
    "method": "GET",
    "path": "/api/v1/platform/organizations/{organizationId}"
  },
  "platform/organizations/update": {
    "method": "PATCH",
    "path": "/api/v1/platform/organizations/{organizationId}"
  },
  "platform/organizations/members/list": {
    "method": "GET",
    "path": "/api/v1/platform/organizations/{organizationId}/members"
  },
  "platform/organizations/members/put": {
    "method": "PUT",
    "path": "/api/v1/platform/organizations/{organizationId}/members/{userId}"
  },
  "platform/organizations/members/delete": {
    "method": "DELETE",
    "path": "/api/v1/platform/organizations/{organizationId}/members/{userId}"
  },
  "platform/organizations/service-users/list": {
    "method": "GET",
    "path": "/api/v1/platform/organizations/{organizationId}/service-users"
  },
  "platform/organizations/service-users/create": {
    "method": "POST",
    "path": "/api/v1/platform/organizations/{organizationId}/service-users"
  },
  "platform/organizations/service-users/update": {
    "method": "PATCH",
    "path": "/api/v1/platform/organizations/{organizationId}/service-users/{serviceUserId}"
  },
  "platform/organizations/service-users/delete": {
    "method": "DELETE",
    "path": "/api/v1/platform/organizations/{organizationId}/service-users/{serviceUserId}"
  },
  "platform/universes/adopt": {
    "method": "POST",
    "path": "/api/v1/platform/organizations/{organizationId}/universes/adopt"
  },
  "platform/universes/create": {
    "method": "POST",
    "path": "/api/v1/platform/organizations/{organizationId}/universes"
  },
  "platform/universes/update": {
    "method": "PATCH",
    "path": "/api/v1/platform/universes/{universe}"
  },
  "platform/universes/grants/list": {
    "method": "GET",
    "path": "/api/v1/platform/universes/{universe}/grants"
  },
  "platform/universes/grants/put": {
    "method": "PUT",
    "path": "/api/v1/platform/universes/{universe}/grants"
  },
  "platform/universes/grants/delete": {
    "method": "DELETE",
    "path": "/api/v1/platform/universes/{universe}/grants/{grantId}"
  },
  "platform/universes/access-policy/put": {
    "method": "PUT",
    "path": "/api/v1/platform/universes/{universe}/access-policy"
  }
} as const;
