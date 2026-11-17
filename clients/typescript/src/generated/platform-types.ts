/* Generated from platform/shared/contract by scripts/generate-platform.mjs. Do not edit. */

export interface PlatformApi {}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformConnectionDetails".
 */
export interface PlatformConnectionDetails {
  principal: {
    kind: "user" | "serviceUser";
    id: string;
    name: string;
    /**
     * Whether the principal is a Platform Admin. Credential restrictions still apply to every operation.
     */
    platformAdmin: boolean;
  };
  credential: {
    kind: "session" | "cliSession" | "personalKey" | "serviceUserKey";
    id: string;
  };
  organizations: {
    id: string;
    name: string;
    role: "member" | "creator" | "admin";
  }[];
  restrictions: {
    /**
     * @maxItems 1000
     */
    organizationIds?: string[];
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
 * via the `definition` "PlatformSessionReadParams".
 */
export interface PlatformSessionReadParams {}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformSessionReadResult".
 */
export interface PlatformSessionReadResult {
  principal: {
    kind: "user" | "serviceUser";
    id: string;
    name: string;
    email: string | null;
  };
  credential: {
    kind: "session" | "cliSession" | "personalKey" | "serviceUserKey";
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
    /**
     * Null for a key that never expires.
     */
    expiresAt: string | null;
    enabled: boolean;
    lastUsedAt: string | null;
    restrictions: {
      /**
       * @maxItems 1000
       */
      organizationIds?: string[];
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
  /**
   * Days until the key expires. Omitted uses the deployment default; null creates a key that never expires, which a deployment maximum refuses.
   */
  expiresInDays?: number | null;
  restrictions?: {
    /**
     * @maxItems 1000
     */
    organizationIds?: string[];
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
    /**
     * Null for a key that never expires.
     */
    expiresAt: string | null;
    enabled: boolean;
    lastUsedAt: string | null;
    restrictions: {
      /**
       * @maxItems 1000
       */
      organizationIds?: string[];
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
 * via the `definition` "PlatformOrganizationsCredentialsListParams".
 */
export interface PlatformOrganizationsCredentialsListParams {
  organizationId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformOrganizationsCredentialsListResult".
 */
export interface PlatformOrganizationsCredentialsListResult {
  /**
   * Keys that can reach the organization: its service users' keys, and personal keys of people with a role in it whose restrictions allow it.
   */
  credentials: {
    id: string;
    name: string;
    prefix: string | null;
    start: string | null;
    createdAt: string;
    /**
     * Null for a key that never expires.
     */
    expiresAt: string | null;
    enabled: boolean;
    lastUsedAt: string | null;
    restrictions: {
      /**
       * @maxItems 1000
       */
      organizationIds?: string[];
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
    owner:
      | {
          kind: "user";
          id: string;
          name: string;
          email: string;
        }
      | {
          kind: "serviceUser";
          id: string;
          name: string;
          enabled: boolean;
        };
  }[];
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformServiceUsersCredentialsListParams".
 */
export interface PlatformServiceUsersCredentialsListParams {
  organizationId: string;
  serviceUserId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformServiceUsersCredentialsListResult".
 */
export interface PlatformServiceUsersCredentialsListResult {
  credentials: {
    id: string;
    name: string;
    prefix: string | null;
    start: string | null;
    createdAt: string;
    /**
     * Null for a key that never expires.
     */
    expiresAt: string | null;
    enabled: boolean;
    lastUsedAt: string | null;
    restrictions: {
      /**
       * @maxItems 1000
       */
      organizationIds?: string[];
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
 * via the `definition` "PlatformServiceUsersCredentialsCreateParams".
 */
export interface PlatformServiceUsersCredentialsCreateParams {
  name: string;
  /**
   * Days until the key expires. Omitted uses the deployment default; null creates a key that never expires, which a deployment maximum refuses.
   */
  expiresInDays?: number | null;
  restrictions?: {
    /**
     * @maxItems 1000
     */
    organizationIds?: string[];
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
  organizationId: string;
  serviceUserId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformServiceUsersCredentialsCreateResult".
 */
export interface PlatformServiceUsersCredentialsCreateResult {
  credential: {
    id: string;
    name: string;
    prefix: string | null;
    start: string | null;
    createdAt: string;
    /**
     * Null for a key that never expires.
     */
    expiresAt: string | null;
    enabled: boolean;
    lastUsedAt: string | null;
    restrictions: {
      /**
       * @maxItems 1000
       */
      organizationIds?: string[];
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
 * via the `definition` "PlatformServiceUsersCredentialsRevokeParams".
 */
export interface PlatformServiceUsersCredentialsRevokeParams {
  organizationId: string;
  serviceUserId: string;
  keyId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformServiceUsersCredentialsRevokeResult".
 */
export interface PlatformServiceUsersCredentialsRevokeResult {
  revoked: true;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformSettingsListParams".
 */
export interface PlatformSettingsListParams {}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformSettingsListResult".
 */
export interface PlatformSettingsListResult {
  settings: {
    key: "personalOrganizations" | "personalUniverseQuota" | "sharedUniverseQuota";
    /**
     * The value in effect.
     */
    value: boolean | number;
    /**
     * Where the value comes from: the deployment's environment (which cannot be changed here), a value set here, or the built-in default.
     */
    source: "environment" | "platform" | "default";
    defaultValue: boolean | number;
    /**
     * The environment variable that sets this value for the whole deployment.
     */
    environmentVariable: string;
    /**
     * When the value set here was last changed.
     */
    updatedAt: string | null;
  }[];
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformSettingsPutParams".
 */
export interface PlatformSettingsPutParams {
  key: "personalOrganizations" | "personalUniverseQuota" | "sharedUniverseQuota";
  /**
   * The new value; null returns the setting to its default.
   */
  value: (boolean | number) | null;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformSettingsPutResult".
 */
export interface PlatformSettingsPutResult {
  setting: {
    key: "personalOrganizations" | "personalUniverseQuota" | "sharedUniverseQuota";
    /**
     * The value in effect.
     */
    value: boolean | number;
    /**
     * Where the value comes from: the deployment's environment (which cannot be changed here), a value set here, or the built-in default.
     */
    source: "environment" | "platform" | "default";
    defaultValue: boolean | number;
    /**
     * The environment variable that sets this value for the whole deployment.
     */
    environmentVariable: string;
    /**
     * When the value set here was last changed.
     */
    updatedAt: string | null;
  };
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformOrganizationsListParams".
 */
export interface PlatformOrganizationsListParams {}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformOrganizationsListResult".
 */
export interface PlatformOrganizationsListResult {
  organizations: {
    id: string;
    name: string;
    slug: string;
    personal: boolean;
    /**
     * The highest organization role the caller holds, for display.
     */
    role: ("member" | "creator" | "admin") | null;
    /**
     * What the caller may do in the organization: the union of every role it holds there.
     */
    actions: ("createUniverse" | "administerUniverses" | "manageMembers" | "manageServiceUsers" | "managePolicy")[];
    /**
     * How many universes the organization may hold.
     */
    universeQuota: number;
    /**
     * Whether the organization has its own universe limit, or follows the deployment default for its kind.
     */
    universeQuotaSource: "organization" | "deployment";
    universeCount?: number;
    /**
     * The universe role every member person holds on every universe in the organization; null gives members access only through grants.
     */
    baseUniverseRole: ("viewer" | "contributor" | "operator" | "owner") | null;
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
 * via the `definition` "PlatformOrganizationsCreateParams".
 */
export interface PlatformOrganizationsCreateParams {
  name: string;
  slug: string;
  initialAdminUserId?: string;
  /**
   * The organization's own universe limit; omitted or null follows the deployment default.
   */
  universeQuota?: number | null;
  baseUniverseRole?: ("viewer" | "contributor" | "operator" | "owner") | null;
  defaultProtected?: boolean;
  requireProtected?: boolean;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformOrganizationsCreateResult".
 */
export interface PlatformOrganizationsCreateResult {
  organization: {
    id: string;
    name: string;
    slug: string;
    personal: boolean;
    /**
     * The highest organization role the caller holds, for display.
     */
    role: ("member" | "creator" | "admin") | null;
    /**
     * What the caller may do in the organization: the union of every role it holds there.
     */
    actions: ("createUniverse" | "administerUniverses" | "manageMembers" | "manageServiceUsers" | "managePolicy")[];
    /**
     * How many universes the organization may hold.
     */
    universeQuota: number;
    /**
     * Whether the organization has its own universe limit, or follows the deployment default for its kind.
     */
    universeQuotaSource: "organization" | "deployment";
    universeCount?: number;
    /**
     * The universe role every member person holds on every universe in the organization; null gives members access only through grants.
     */
    baseUniverseRole: ("viewer" | "contributor" | "operator" | "owner") | null;
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
 * via the `definition` "PlatformOrganizationsReadParams".
 */
export interface PlatformOrganizationsReadParams {
  organizationId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformOrganizationsReadResult".
 */
export interface PlatformOrganizationsReadResult {
  organization: {
    id: string;
    name: string;
    slug: string;
    personal: boolean;
    /**
     * The highest organization role the caller holds, for display.
     */
    role: ("member" | "creator" | "admin") | null;
    /**
     * What the caller may do in the organization: the union of every role it holds there.
     */
    actions: ("createUniverse" | "administerUniverses" | "manageMembers" | "manageServiceUsers" | "managePolicy")[];
    /**
     * How many universes the organization may hold.
     */
    universeQuota: number;
    /**
     * Whether the organization has its own universe limit, or follows the deployment default for its kind.
     */
    universeQuotaSource: "organization" | "deployment";
    universeCount?: number;
    /**
     * The universe role every member person holds on every universe in the organization; null gives members access only through grants.
     */
    baseUniverseRole: ("viewer" | "contributor" | "operator" | "owner") | null;
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
 * via the `definition` "PlatformOrganizationsUpdateParams".
 */
export interface PlatformOrganizationsUpdateParams {
  name?: string;
  /**
   * The organization's own universe limit; null returns it to the deployment default.
   */
  universeQuota?: number | null;
  baseUniverseRole?: ("viewer" | "contributor" | "operator" | "owner") | null;
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
  organizationId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformOrganizationsUpdateResult".
 */
export interface PlatformOrganizationsUpdateResult {
  organization: {
    id: string;
    name: string;
    slug: string;
    personal: boolean;
    /**
     * The highest organization role the caller holds, for display.
     */
    role: ("member" | "creator" | "admin") | null;
    /**
     * What the caller may do in the organization: the union of every role it holds there.
     */
    actions: ("createUniverse" | "administerUniverses" | "manageMembers" | "manageServiceUsers" | "managePolicy")[];
    /**
     * How many universes the organization may hold.
     */
    universeQuota: number;
    /**
     * Whether the organization has its own universe limit, or follows the deployment default for its kind.
     */
    universeQuotaSource: "organization" | "deployment";
    universeCount?: number;
    /**
     * The universe role every member person holds on every universe in the organization; null gives members access only through grants.
     */
    baseUniverseRole: ("viewer" | "contributor" | "operator" | "owner") | null;
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
 * via the `definition` "PlatformOrganizationsMembersListParams".
 */
export interface PlatformOrganizationsMembersListParams {
  organizationId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformOrganizationsMembersListResult".
 */
export interface PlatformOrganizationsMembersListResult {
  members: {
    id: string;
    userId: string;
    name: string;
    email: string;
    role: "member" | "creator" | "admin";
    /**
     * Holds an explicit membership that an admin added; removing the member removes it.
     */
    added: boolean;
    /**
     * The organization's membership policy or a directory group grants membership; only changing that policy removes it.
     */
    declared: boolean;
    createdAt: string;
  }[];
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformOrganizationsMembersPutParams".
 */
export interface PlatformOrganizationsMembersPutParams {
  userId: string;
  role?: "member" | "creator" | "admin";
  organizationId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformOrganizationsMembersPutResult".
 */
export interface PlatformOrganizationsMembersPutResult {
  member: {
    id: string;
    userId: string;
    name: string;
    email: string;
    role: "member" | "creator" | "admin";
    /**
     * Holds an explicit membership that an admin added; removing the member removes it.
     */
    added: boolean;
    /**
     * The organization's membership policy or a directory group grants membership; only changing that policy removes it.
     */
    declared: boolean;
    createdAt: string;
  };
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformOrganizationsMembersDeleteParams".
 */
export interface PlatformOrganizationsMembersDeleteParams {
  organizationId: string;
  userId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformOrganizationsMembersDeleteResult".
 */
export interface PlatformOrganizationsMembersDeleteResult {
  ok: true;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformOrganizationsServiceUsersListParams".
 */
export interface PlatformOrganizationsServiceUsersListParams {
  organizationId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformOrganizationsServiceUsersListResult".
 */
export interface PlatformOrganizationsServiceUsersListResult {
  serviceUsers: {
    id: string;
    organizationId: string;
    name: string;
    enabled: boolean;
    role: "member" | "creator" | "admin";
    createdAt: string;
  }[];
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformOrganizationsServiceUsersCreateParams".
 */
export interface PlatformOrganizationsServiceUsersCreateParams {
  name: string;
  role?: "member" | "creator" | "admin";
  organizationId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformOrganizationsServiceUsersCreateResult".
 */
export interface PlatformOrganizationsServiceUsersCreateResult {
  serviceUser: {
    id: string;
    organizationId: string;
    name: string;
    enabled: boolean;
    role: "member" | "creator" | "admin";
    createdAt: string;
  };
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformOrganizationsServiceUsersUpdateParams".
 */
export interface PlatformOrganizationsServiceUsersUpdateParams {
  name?: string;
  enabled?: boolean;
  role?: "member" | "creator" | "admin";
  organizationId: string;
  serviceUserId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformOrganizationsServiceUsersUpdateResult".
 */
export interface PlatformOrganizationsServiceUsersUpdateResult {
  serviceUser: {
    id: string;
    organizationId: string;
    name: string;
    enabled: boolean;
    role: "member" | "creator" | "admin";
    createdAt: string;
  };
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformOrganizationsServiceUsersDeleteParams".
 */
export interface PlatformOrganizationsServiceUsersDeleteParams {
  organizationId: string;
  serviceUserId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformOrganizationsServiceUsersDeleteResult".
 */
export interface PlatformOrganizationsServiceUsersDeleteResult {
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
    kind: "user" | "serviceUser";
    id: string;
  };
  organizationId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformUniversesAdoptResult".
 */
export interface PlatformUniversesAdoptResult {
  universe: {
    id: string;
    organizationId: string;
    lightspeedUniverseId: string;
    name: string;
    handle: string;
    role: "viewer" | "contributor" | "operator" | "owner";
    organizationRole: ("member" | "creator" | "admin") | null;
    status: "provisioning" | "active" | "archived" | "deleting";
    protected: boolean;
    expiresAtMs: number | null;
    createdBy: string | null;
    features: {
      [k: string]: boolean;
    };
    icon: string;
    iconColor: string;
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
    kind: "user" | "serviceUser";
    id: string;
  };
  metadata?: {
    [k: string]: string;
  };
  protected?: boolean;
  expiresAtMs?: number | null;
  organizationId: string;
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformUniversesCreateResult".
 */
export interface PlatformUniversesCreateResult {
  universe: {
    id: string;
    organizationId: string;
    lightspeedUniverseId: string;
    name: string;
    handle: string;
    role: "viewer" | "contributor" | "operator" | "owner";
    organizationRole: ("member" | "creator" | "admin") | null;
    status: "provisioning" | "active" | "archived" | "deleting";
    protected: boolean;
    expiresAtMs: number | null;
    createdBy: string | null;
    features: {
      [k: string]: boolean;
    };
    icon: string;
    iconColor: string;
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
    organizationId: string;
    lightspeedUniverseId: string;
    name: string;
    handle: string;
    role: "viewer" | "contributor" | "operator" | "owner";
    organizationRole: ("member" | "creator" | "admin") | null;
    status: "provisioning" | "active" | "archived" | "deleting";
    protected: boolean;
    expiresAtMs: number | null;
    createdBy: string | null;
    features: {
      [k: string]: boolean;
    };
    icon: string;
    iconColor: string;
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
      kind: "user" | "serviceUser";
      id: string;
    };
    role: "viewer" | "contributor" | "operator" | "owner";
    name: string;
    active: boolean;
    createdAt: string;
  }[];
}
/**
 * This interface was referenced by `PlatformApi`'s JSON-Schema
 * via the `definition` "PlatformUniversesGrantsPutParams".
 */
export interface PlatformUniversesGrantsPutParams {
  principal: {
    kind: "user" | "serviceUser";
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
      kind: "user" | "serviceUser";
      id: string;
    };
    role: "viewer" | "contributor" | "operator" | "owner";
    name: string;
    active: boolean;
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
