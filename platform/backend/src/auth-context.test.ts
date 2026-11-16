import { expect, it } from "vitest";
import type { AuthContext } from "./auth-context.js";
import { enforceRestrictions, requireCredentialManagement } from "./auth-context.js";
import { principalAllowed } from "./auth-access.js";
import { identityEnv } from "../test/identity-fixture.js";

const administrator = (restrictions: AuthContext["credential"]["restrictions"]): AuthContext => ({
  principal: { kind: "user", id: "u", user: {} as never, platformAdmin: true },
  actorId: "u", credential: { kind: "personalKey", id: "key", restrictions },
});
it("keeps credential ceilings in force for deployment administrators", () => {
  const auth = administrator({ accountIds: ["a"], universeIds: ["v"], operations: ["profiles/read"], readOnly: true });
  expect(() => enforceRestrictions(auth, { operation: "profiles/read", accountId: "a", universeId: "v" })).not.toThrow();
  for (const target of [
    { operation: "profiles/put", accountId: "a", universeId: "v", write: true },
    { operation: "profiles/read", accountId: "other", universeId: "v" },
    { operation: "profiles/read", accountId: "a", universeId: "other" },
    { operation: "profiles/read", accountId: "a", createUniverse: true },
  ]) expect(() => enforceRestrictions(auth, target)).toThrow();
});
it("universe-scoped credentials cannot mutate their containing account or create universes", () => {
  const auth = administrator({ universeIds: ["v"], allowUniverseCreation: true });
  expect(() => enforceRestrictions(auth, { operation: "platform/accounts/update", accountId: "a", write: true })).toThrow();
  expect(() => enforceRestrictions(auth, { operation: "platform/universes/create", accountId: "a", createUniverse: true })).toThrow();
});
it("API keys cannot mint broader credentials even when unrestricted", () => {
  expect(() => requireCredentialManagement(administrator({}))).toThrow();
  const auth = administrator({}); auth.credential.kind = "applicationKey";
  expect(() => requireCredentialManagement(auth)).toThrow();
});
it("company keys depend on current admission freshness, independently of a browser expiry", () => {
  const now = Date.now();
  const company = { identitySource: "company", oidcIssuer: identityEnv.oidc!.issuer, companyAdmitted: true, providerCheckedAt: new Date(now - 1000) };
  expect(principalAllowed(company, identityEnv, now)).toBe(true);
  expect(principalAllowed({ ...company, providerCheckedAt: new Date(now - identityEnv.oidc!.sessionMaxAgeSeconds * 1000) }, identityEnv, now)).toBe(false);
  expect(principalAllowed({ ...company, companyAdmitted: false }, identityEnv, now)).toBe(false);
  expect(principalAllowed({ ...company, banned: true }, identityEnv, now)).toBe(false);
  expect(principalAllowed({ ...company, providerCheckedAt: new Date(now + 1) }, identityEnv, now)).toBe(false);
});
