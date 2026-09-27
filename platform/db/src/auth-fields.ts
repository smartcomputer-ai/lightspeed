// Better Auth's field metadata mirrors the owned Drizzle columns. Keep these
// server-controlled: neither sign-up nor profile edits may set them.
export const identityUserFields = {
  identitySource: { type: "string", required: false, defaultValue: "local", input: false },
  oidcIssuer: { type: "string", required: false, input: false },
  oidcSubject: { type: "string", required: false, input: false },
  companyAdmitted: { type: "boolean", required: false, defaultValue: false, input: false },
  providerCheckedAt: { type: "date", required: false, input: false },
  emergencyAdmin: { type: "boolean", required: false, defaultValue: false, input: false },
  accessVersion: { type: "number", required: false, defaultValue: 0, input: false },
} as const;
export const identitySessionFields = {
  accessVersion: { type: "number", required: false, defaultValue: 0, input: false },
} as const;
