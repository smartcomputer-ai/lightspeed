/* Generated from platform/shared/contract by scripts/generate-platform.mjs. Do not edit. */

export interface PlatformApi {}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformSessionReadParams".
 */
export interface PlatformSessionReadParams {}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformSessionReadResult".
 */
export interface PlatformSessionReadResult {
  principal: {
    kind: "user" | "application";
    id: string;
    name: string;
    email: string | null;
  };
  credential: {
    kind: "session" | "cliSession" | "personalKey" | "applicationKey";
    id: string;
  };
  session: {
    id: string;
    createdAt: string;
    expiresAt: string;
  } | null;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformSessionRevokeParams".
 */
export interface PlatformSessionRevokeParams {}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformSessionRevokeResult".
 */
export interface PlatformSessionRevokeResult {
  revoked: true;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformCredentialsListParams".
 */
export interface PlatformCredentialsListParams {}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformCredentialsListResult".
 */
export interface PlatformCredentialsListResult {
  credentials: {
    id: string;
    name: string;
    prefix: string | null;
    start: string | null;
    createdAt: string;
    expiresAt: string;
    enabled: boolean;
    lastUsedAt: string | null;
    restrictions: {
      /**
       * @maxItems 1000
       */
      accountIds?: string[];
      /**
       * @maxItems 1000
       */
      universeIds?: string[];
      /**
       * @maxItems 1000
       */
      operations?: string[];
      readOnly?: boolean;
      allowUniverseCreation?: boolean;
    };
  }[];
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformCredentialsCreateParams".
 */
export interface PlatformCredentialsCreateParams {
  name: string;
  expiresInDays?: number;
  restrictions?: {
    /**
     * @maxItems 1000
     */
    accountIds?: string[];
    /**
     * @maxItems 1000
     */
    universeIds?: string[];
    /**
     * @maxItems 1000
     */
    operations?: string[];
    readOnly?: boolean;
    allowUniverseCreation?: boolean;
  };
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformCredentialsCreateResult".
 */
export interface PlatformCredentialsCreateResult {
  credential: {
    id: string;
    name: string;
    prefix: string | null;
    start: string | null;
    createdAt: string;
    expiresAt: string;
    enabled: boolean;
    lastUsedAt: string | null;
    restrictions: {
      /**
       * @maxItems 1000
       */
      accountIds?: string[];
      /**
       * @maxItems 1000
       */
      universeIds?: string[];
      /**
       * @maxItems 1000
       */
      operations?: string[];
      readOnly?: boolean;
      allowUniverseCreation?: boolean;
    };
  };
  secret: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformCredentialsRevokeParams".
 */
export interface PlatformCredentialsRevokeParams {
  keyId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformCredentialsRevokeResult".
 */
export interface PlatformCredentialsRevokeResult {
  revoked: true;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformApplicationsCredentialsListParams".
 */
export interface PlatformApplicationsCredentialsListParams {
  accountId: string;
  applicationId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformApplicationsCredentialsListResult".
 */
export interface PlatformApplicationsCredentialsListResult {
  credentials: {
    id: string;
    name: string;
    prefix: string | null;
    start: string | null;
    createdAt: string;
    expiresAt: string;
    enabled: boolean;
    lastUsedAt: string | null;
    restrictions: {
      /**
       * @maxItems 1000
       */
      accountIds?: string[];
      /**
       * @maxItems 1000
       */
      universeIds?: string[];
      /**
       * @maxItems 1000
       */
      operations?: string[];
      readOnly?: boolean;
      allowUniverseCreation?: boolean;
    };
  }[];
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformApplicationsCredentialsCreateParams".
 */
export interface PlatformApplicationsCredentialsCreateParams {
  name: string;
  expiresInDays?: number;
  restrictions?: {
    /**
     * @maxItems 1000
     */
    accountIds?: string[];
    /**
     * @maxItems 1000
     */
    universeIds?: string[];
    /**
     * @maxItems 1000
     */
    operations?: string[];
    readOnly?: boolean;
    allowUniverseCreation?: boolean;
  };
  accountId: string;
  applicationId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformApplicationsCredentialsCreateResult".
 */
export interface PlatformApplicationsCredentialsCreateResult {
  credential: {
    id: string;
    name: string;
    prefix: string | null;
    start: string | null;
    createdAt: string;
    expiresAt: string;
    enabled: boolean;
    lastUsedAt: string | null;
    restrictions: {
      /**
       * @maxItems 1000
       */
      accountIds?: string[];
      /**
       * @maxItems 1000
       */
      universeIds?: string[];
      /**
       * @maxItems 1000
       */
      operations?: string[];
      readOnly?: boolean;
      allowUniverseCreation?: boolean;
    };
  };
  secret: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformApplicationsCredentialsRevokeParams".
 */
export interface PlatformApplicationsCredentialsRevokeParams {
  accountId: string;
  applicationId: string;
  keyId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformApplicationsCredentialsRevokeResult".
 */
export interface PlatformApplicationsCredentialsRevokeResult {
  revoked: true;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsListParams".
 */
export interface PlatformAccountsListParams {}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsListResult".
 */
export interface PlatformAccountsListResult {
  accounts: {
    id: string;
    name: string;
    slug: string;
    kind: "personal" | "shared" | "development";
    enabled: boolean;
    role: ("member" | "creator" | "admin") | null;
    universeQuota: number;
    defaultProtected: boolean;
    requireProtected: boolean;
    membershipPolicy?: {
      /**
       * @maxItems 1000
       */
      members?: (
        | {
            email: string;
            role: "member" | "creator" | "admin";
          }
        | {
            issuer: string;
            subject: string;
            role: "member" | "creator" | "admin";
          }
      )[];
      /**
       * @maxItems 1000
       */
      directoryGroupMappings?: {
        issuer: string;
        group: string;
        role: "member" | "creator" | "admin";
      }[];
    };
    createdAt: string;
  }[];
  canCreate: boolean;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsCreateParams".
 */
export interface PlatformAccountsCreateParams {
  name: string;
  slug: string;
  kind?: "shared" | "development";
  initialAdminUserId?: string;
  universeQuota?: number;
  defaultProtected?: boolean;
  requireProtected?: boolean;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsCreateResult".
 */
export interface PlatformAccountsCreateResult {
  account: {
    id: string;
    name: string;
    slug: string;
    kind: "personal" | "shared" | "development";
    enabled: boolean;
    role: ("member" | "creator" | "admin") | null;
    universeQuota: number;
    defaultProtected: boolean;
    requireProtected: boolean;
    membershipPolicy?: {
      /**
       * @maxItems 1000
       */
      members?: (
        | {
            email: string;
            role: "member" | "creator" | "admin";
          }
        | {
            issuer: string;
            subject: string;
            role: "member" | "creator" | "admin";
          }
      )[];
      /**
       * @maxItems 1000
       */
      directoryGroupMappings?: {
        issuer: string;
        group: string;
        role: "member" | "creator" | "admin";
      }[];
    };
    createdAt: string;
  };
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsReadParams".
 */
export interface PlatformAccountsReadParams {
  accountId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsReadResult".
 */
export interface PlatformAccountsReadResult {
  account: {
    id: string;
    name: string;
    slug: string;
    kind: "personal" | "shared" | "development";
    enabled: boolean;
    role: ("member" | "creator" | "admin") | null;
    universeQuota: number;
    defaultProtected: boolean;
    requireProtected: boolean;
    membershipPolicy?: {
      /**
       * @maxItems 1000
       */
      members?: (
        | {
            email: string;
            role: "member" | "creator" | "admin";
          }
        | {
            issuer: string;
            subject: string;
            role: "member" | "creator" | "admin";
          }
      )[];
      /**
       * @maxItems 1000
       */
      directoryGroupMappings?: {
        issuer: string;
        group: string;
        role: "member" | "creator" | "admin";
      }[];
    };
    createdAt: string;
  };
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsUpdateParams".
 */
export interface PlatformAccountsUpdateParams {
  name?: string;
  enabled?: boolean;
  universeQuota?: number;
  defaultProtected?: boolean;
  requireProtected?: boolean;
  membershipPolicy?: {
    /**
     * @maxItems 1000
     */
    members?: (
      | {
          email: string;
          role: "member" | "creator" | "admin";
        }
      | {
          issuer: string;
          subject: string;
          role: "member" | "creator" | "admin";
        }
    )[];
    /**
     * @maxItems 1000
     */
    directoryGroupMappings?: {
      issuer: string;
      group: string;
      role: "member" | "creator" | "admin";
    }[];
  };
  accountId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsUpdateResult".
 */
export interface PlatformAccountsUpdateResult {
  account: {
    id: string;
    name: string;
    slug: string;
    kind: "personal" | "shared" | "development";
    enabled: boolean;
    role: ("member" | "creator" | "admin") | null;
    universeQuota: number;
    defaultProtected: boolean;
    requireProtected: boolean;
    membershipPolicy?: {
      /**
       * @maxItems 1000
       */
      members?: (
        | {
            email: string;
            role: "member" | "creator" | "admin";
          }
        | {
            issuer: string;
            subject: string;
            role: "member" | "creator" | "admin";
          }
      )[];
      /**
       * @maxItems 1000
       */
      directoryGroupMappings?: {
        issuer: string;
        group: string;
        role: "member" | "creator" | "admin";
      }[];
    };
    createdAt: string;
  };
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsMembersListParams".
 */
export interface PlatformAccountsMembersListParams {
  accountId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsMembersListResult".
 */
export interface PlatformAccountsMembersListResult {
  members: {
    id: string;
    userId: string;
    name: string;
    email: string;
    role: "member" | "creator" | "admin";
    createdAt: string;
  }[];
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsMembersPutParams".
 */
export interface PlatformAccountsMembersPutParams {
  userId: string;
  role?: "member" | "creator" | "admin";
  accountId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsMembersPutResult".
 */
export interface PlatformAccountsMembersPutResult {
  member: {
    id: string;
    userId: string;
    name: string;
    email: string;
    role: "member" | "creator" | "admin";
    createdAt: string;
  };
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsMembersDeleteParams".
 */
export interface PlatformAccountsMembersDeleteParams {
  accountId: string;
  userId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsMembersDeleteResult".
 */
export interface PlatformAccountsMembersDeleteResult {
  ok: true;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsApplicationsListParams".
 */
export interface PlatformAccountsApplicationsListParams {
  accountId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsApplicationsListResult".
 */
export interface PlatformAccountsApplicationsListResult {
  applications: {
    id: string;
    accountId: string;
    name: string;
    enabled: boolean;
    role: "member" | "creator" | "admin";
    createdAt: string;
  }[];
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsApplicationsCreateParams".
 */
export interface PlatformAccountsApplicationsCreateParams {
  name: string;
  role?: "member" | "creator" | "admin";
  accountId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsApplicationsCreateResult".
 */
export interface PlatformAccountsApplicationsCreateResult {
  application: {
    id: string;
    accountId: string;
    name: string;
    enabled: boolean;
    role: "member" | "creator" | "admin";
    createdAt: string;
  };
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsApplicationsUpdateParams".
 */
export interface PlatformAccountsApplicationsUpdateParams {
  name?: string;
  enabled?: boolean;
  role?: "member" | "creator" | "admin";
  accountId: string;
  applicationId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsApplicationsUpdateResult".
 */
export interface PlatformAccountsApplicationsUpdateResult {
  application: {
    id: string;
    accountId: string;
    name: string;
    enabled: boolean;
    role: "member" | "creator" | "admin";
    createdAt: string;
  };
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsApplicationsDeleteParams".
 */
export interface PlatformAccountsApplicationsDeleteParams {
  accountId: string;
  applicationId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformAccountsApplicationsDeleteResult".
 */
export interface PlatformAccountsApplicationsDeleteResult {
  ok: true;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformUniversesAdoptParams".
 */
export interface PlatformUniversesAdoptParams {
  lightspeedUniverseId: string;
  name: string;
  initialOwner: {
    kind: "user" | "application";
    id: string;
  };
  accountId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformUniversesAdoptResult".
 */
export interface PlatformUniversesAdoptResult {
  universe: {
    id: string;
    accountId: string;
    lightspeedUniverseId: string;
    name: string;
    handle: string;
    role: "viewer" | "contributor" | "operator" | "owner";
    accountRole: ("member" | "creator" | "admin") | null;
    status: "provisioning" | "active" | "archived" | "deleting";
    protected: boolean;
    expiresAtMs: number | null;
    createdBy: string | null;
    features: {
      [k: string]: boolean;
    };
    icon: string;
    iconColor: string;
    gatewayUrl: string | null;
    runtimeConfiguration: {
      [k: string]: unknown;
    };
    createdAt: string;
    updatedAt: string;
  };
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformUniversesCreateParams".
 */
export interface PlatformUniversesCreateParams {
  universeId?: string;
  name: string;
  handle?: string;
  initialOwner?: {
    kind: "user" | "application";
    id: string;
  };
  metadata?: {
    [k: string]: string;
  };
  protected?: boolean;
  expiresAtMs?: number | null;
  accountId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformUniversesCreateResult".
 */
export interface PlatformUniversesCreateResult {
  universe: {
    id: string;
    accountId: string;
    lightspeedUniverseId: string;
    name: string;
    handle: string;
    role: "viewer" | "contributor" | "operator" | "owner";
    accountRole: ("member" | "creator" | "admin") | null;
    status: "provisioning" | "active" | "archived" | "deleting";
    protected: boolean;
    expiresAtMs: number | null;
    createdBy: string | null;
    features: {
      [k: string]: boolean;
    };
    icon: string;
    iconColor: string;
    gatewayUrl: string | null;
    runtimeConfiguration: {
      [k: string]: unknown;
    };
    createdAt: string;
    updatedAt: string;
  };
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformUniversesUpdateParams".
 */
export interface PlatformUniversesUpdateParams {
  name?: string;
  status?: "active" | "archived";
  metadata?: {
    [k: string]: string;
  };
  protected?: boolean;
  expiresAtMs?: number | null;
  universe: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformUniversesUpdateResult".
 */
export interface PlatformUniversesUpdateResult {
  universe: {
    id: string;
    accountId: string;
    lightspeedUniverseId: string;
    name: string;
    handle: string;
    role: "viewer" | "contributor" | "operator" | "owner";
    accountRole: ("member" | "creator" | "admin") | null;
    status: "provisioning" | "active" | "archived" | "deleting";
    protected: boolean;
    expiresAtMs: number | null;
    createdBy: string | null;
    features: {
      [k: string]: boolean;
    };
    icon: string;
    iconColor: string;
    gatewayUrl: string | null;
    runtimeConfiguration: {
      [k: string]: unknown;
    };
    createdAt: string;
    updatedAt: string;
  };
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformUniversesGrantsListParams".
 */
export interface PlatformUniversesGrantsListParams {
  universe: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformUniversesGrantsListResult".
 */
export interface PlatformUniversesGrantsListResult {
  grants: {
    id: string;
    universeId: string;
    principal: {
      kind: "user" | "application";
      id: string;
    };
    role: "viewer" | "contributor" | "operator" | "owner";
    name: string;
    createdAt: string;
  }[];
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformUniversesGrantsPutParams".
 */
export interface PlatformUniversesGrantsPutParams {
  principal: {
    kind: "user" | "application";
    id: string;
  };
  role: "viewer" | "contributor" | "operator" | "owner";
  universe: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformUniversesGrantsPutResult".
 */
export interface PlatformUniversesGrantsPutResult {
  grant: {
    id: string;
    universeId: string;
    principal: {
      kind: "user" | "application";
      id: string;
    };
    role: "viewer" | "contributor" | "operator" | "owner";
    name: string;
    createdAt: string;
  };
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformUniversesGrantsDeleteParams".
 */
export interface PlatformUniversesGrantsDeleteParams {
  universe: string;
  grantId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformUniversesGrantsDeleteResult".
 */
export interface PlatformUniversesGrantsDeleteResult {
  ok: true;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformUniversesAccessPolicyPutParams".
 */
export interface PlatformUniversesAccessPolicyPutParams {
  universe: string;
  policy: {
    /**
     * @maxItems 1000
     */
    members?: (
      | {
          email: string;
          role: "viewer" | "contributor" | "operator" | "owner";
        }
      | {
          issuer: string;
          subject: string;
          role: "viewer" | "contributor" | "operator" | "owner";
        }
    )[];
    /**
     * @maxItems 1000
     */
    directoryGroupMappings?: {
      issuer: string;
      group: string;
      role: "viewer" | "contributor" | "operator" | "owner";
    }[];
  };
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformUniversesAccessPolicyPutResult".
 */
export interface PlatformUniversesAccessPolicyPutResult {
  policy: {
    /**
     * @maxItems 1000
     */
    members?: (
      | {
          email: string;
          role: "viewer" | "contributor" | "operator" | "owner";
        }
      | {
          issuer: string;
          subject: string;
          role: "viewer" | "contributor" | "operator" | "owner";
        }
    )[];
    /**
     * @maxItems 1000
     */
    directoryGroupMappings?: {
      issuer: string;
      group: string;
      role: "viewer" | "contributor" | "operator" | "owner";
    }[];
  };
}
