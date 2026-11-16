/* Generated from platform/shared/contract by scripts/generate-platform.mjs. Do not edit. */
import type * as T from "./platform-types.js";
export interface PlatformOperationMap {
  "platform/session/read": { params: T.PlatformSessionReadParams; result: T.PlatformSessionReadResult };
  "platform/session/revoke": { params: T.PlatformSessionRevokeParams; result: T.PlatformSessionRevokeResult };
  "platform/credentials/list": { params: T.PlatformCredentialsListParams; result: T.PlatformCredentialsListResult };
  "platform/credentials/create": { params: T.PlatformCredentialsCreateParams; result: T.PlatformCredentialsCreateResult };
  "platform/credentials/revoke": { params: T.PlatformCredentialsRevokeParams; result: T.PlatformCredentialsRevokeResult };
  "platform/applications/credentials/list": { params: T.PlatformApplicationsCredentialsListParams; result: T.PlatformApplicationsCredentialsListResult };
  "platform/applications/credentials/create": { params: T.PlatformApplicationsCredentialsCreateParams; result: T.PlatformApplicationsCredentialsCreateResult };
  "platform/applications/credentials/revoke": { params: T.PlatformApplicationsCredentialsRevokeParams; result: T.PlatformApplicationsCredentialsRevokeResult };
  "platform/accounts/list": { params: T.PlatformAccountsListParams; result: T.PlatformAccountsListResult };
  "platform/accounts/create": { params: T.PlatformAccountsCreateParams; result: T.PlatformAccountsCreateResult };
  "platform/accounts/read": { params: T.PlatformAccountsReadParams; result: T.PlatformAccountsReadResult };
  "platform/accounts/update": { params: T.PlatformAccountsUpdateParams; result: T.PlatformAccountsUpdateResult };
  "platform/accounts/members/list": { params: T.PlatformAccountsMembersListParams; result: T.PlatformAccountsMembersListResult };
  "platform/accounts/members/put": { params: T.PlatformAccountsMembersPutParams; result: T.PlatformAccountsMembersPutResult };
  "platform/accounts/members/delete": { params: T.PlatformAccountsMembersDeleteParams; result: T.PlatformAccountsMembersDeleteResult };
  "platform/accounts/applications/list": { params: T.PlatformAccountsApplicationsListParams; result: T.PlatformAccountsApplicationsListResult };
  "platform/accounts/applications/create": { params: T.PlatformAccountsApplicationsCreateParams; result: T.PlatformAccountsApplicationsCreateResult };
  "platform/accounts/applications/update": { params: T.PlatformAccountsApplicationsUpdateParams; result: T.PlatformAccountsApplicationsUpdateResult };
  "platform/accounts/applications/delete": { params: T.PlatformAccountsApplicationsDeleteParams; result: T.PlatformAccountsApplicationsDeleteResult };
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
  "platform/applications/credentials/list": {
    "method": "GET",
    "path": "/api/v1/platform/accounts/{accountId}/applications/{applicationId}/credentials"
  },
  "platform/applications/credentials/create": {
    "method": "POST",
    "path": "/api/v1/platform/accounts/{accountId}/applications/{applicationId}/credentials"
  },
  "platform/applications/credentials/revoke": {
    "method": "DELETE",
    "path": "/api/v1/platform/accounts/{accountId}/applications/{applicationId}/credentials/{keyId}"
  },
  "platform/accounts/list": {
    "method": "GET",
    "path": "/api/v1/platform/accounts"
  },
  "platform/accounts/create": {
    "method": "POST",
    "path": "/api/v1/platform/accounts"
  },
  "platform/accounts/read": {
    "method": "GET",
    "path": "/api/v1/platform/accounts/{accountId}"
  },
  "platform/accounts/update": {
    "method": "PATCH",
    "path": "/api/v1/platform/accounts/{accountId}"
  },
  "platform/accounts/members/list": {
    "method": "GET",
    "path": "/api/v1/platform/accounts/{accountId}/members"
  },
  "platform/accounts/members/put": {
    "method": "PUT",
    "path": "/api/v1/platform/accounts/{accountId}/members/{userId}"
  },
  "platform/accounts/members/delete": {
    "method": "DELETE",
    "path": "/api/v1/platform/accounts/{accountId}/members/{userId}"
  },
  "platform/accounts/applications/list": {
    "method": "GET",
    "path": "/api/v1/platform/accounts/{accountId}/applications"
  },
  "platform/accounts/applications/create": {
    "method": "POST",
    "path": "/api/v1/platform/accounts/{accountId}/applications"
  },
  "platform/accounts/applications/update": {
    "method": "PATCH",
    "path": "/api/v1/platform/accounts/{accountId}/applications/{applicationId}"
  },
  "platform/accounts/applications/delete": {
    "method": "DELETE",
    "path": "/api/v1/platform/accounts/{accountId}/applications/{applicationId}"
  },
  "platform/universes/adopt": {
    "method": "POST",
    "path": "/api/v1/platform/accounts/{accountId}/universes/adopt"
  },
  "platform/universes/create": {
    "method": "POST",
    "path": "/api/v1/platform/accounts/{accountId}/universes"
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
