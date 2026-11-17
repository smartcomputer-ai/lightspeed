// Lightspeed's columns on Better Auth's tables, declared to Better Auth so
// its view of each table matches the Drizzle schema (checked by
// auth-schema.test.ts). Keep these server-controlled: neither sign-up nor
// profile edits may set them.
export const identityUserFields = {
  identitySource: { type: "string", required: false, defaultValue: "local", input: false },
  oidcIssuer: { type: "string", required: false, input: false },
  oidcSubject: { type: "string", required: false, input: false },
  companyAdmitted: { type: "boolean", required: false, defaultValue: false, input: false },
  providerCheckedAt: { type: "date", required: false, input: false },
  emergencyAdmin: { type: "boolean", required: false, defaultValue: false, input: false },
  accessVersion: { type: "number", required: false, defaultValue: 0, input: false },
  // Written only by company sign-in; kept out of Better Auth's responses.
  directoryGroups: { type: "json", required: false, input: false, returned: false },
} as const;
export const identitySessionFields = {
  accessVersion: { type: "number", required: false, defaultValue: 0, input: false },
  sessionKind: { type: "string", required: false, defaultValue: "browser", input: false },
} as const;

export const deviceCodeFields = {
  approvalAccessVersion: { type: "number", required: false, input: false },
} as const;

export const organizationFields = {
  personalUserId: { type: "string", required: false, input: false },
  universeQuota: { type: "number", required: false, input: false },
  baseUniverseRole: { type: "string", required: false, input: false },
  defaultProtected: { type: "boolean", required: false, defaultValue: true, input: false },
  requireProtected: { type: "boolean", required: false, defaultValue: false, input: false },
  membershipPolicy: { type: "json", required: false, input: false },
} as const;
