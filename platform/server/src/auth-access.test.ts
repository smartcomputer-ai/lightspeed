import { describe, expect, it } from "vitest";
import { identityEnv } from "../test/identity-fixture.js";
import { canUsePassword, sessionAllowed } from "./auth-access.js";
import { applicationAccess, companyProviderId } from "./company-identity.js";

const now = Date.now();
const record = { createdAt: new Date(now - 1000), expiresAt: new Date(now + 1000), accessVersion: 2 };
const company = { identitySource: "company", oidcIssuer: identityEnv.oidc!.issuer, companyAdmitted: true, accessVersion: 2 };

describe("session access", () => {
  it("keeps invalidated sessions invalid after reinstatement, including a racing session insert", () => {
    expect(sessionAllowed({ session: record, user: company }, identityEnv, now)).toBe(true);
    expect(sessionAllowed({ session: record, user: { ...company, banned: true } }, identityEnv, now)).toBe(false);
    expect(sessionAllowed({ session: record, user: { ...company, banned: false, accessVersion: 3 } }, identityEnv, now)).toBe(false);
  });
  it("does not reuse company admission after disabling SSO or changing issuer", () => {
    expect(sessionAllowed({ session: record, user: company }, { ...identityEnv, oidc: null }, now)).toBe(false);
    expect(sessionAllowed({ session: record, user: company }, { ...identityEnv, oidc: { ...identityEnv.oidc!, issuer: "https://other.test" } }, now)).toBe(false);
    expect(companyProviderId(identityEnv.oidc!)).not.toBe(companyProviderId({ ...identityEnv.oidc!, issuer: "https://other.test" }));
  });
  it("enforces the configured absolute age even on sessions created before enabling SSO", () => {
    expect(sessionAllowed({ session: { ...record, createdAt: new Date(now - 28800001) }, user: company }, identityEnv, now)).toBe(false);
  });
  it("requires explicit local emergency designation, not merely the admin role", () => {
    expect(canUsePassword({ role: "admin" }, identityEnv)).toBe(false);
    expect(canUsePassword({ role: "admin", emergencyAdmin: true }, identityEnv)).toBe(true);
    expect(canUsePassword({ ...company, role: "admin", emergencyAdmin: true }, identityEnv)).toBe(false);
    expect(canUsePassword({ role: "user" }, { ...identityEnv, oidc: null })).toBe(true);
  });
  it("matches only exact opaque entitlement values", () => {
    expect(applicationAccess({ groups: "lightspeed-admins" }, identityEnv.oidc!)).toEqual({ admitted: true, admin: true });
    for (const groups of ["lightspeed-users,lightspeed-admins", ["prefix-lightspeed-admins"], ["LIGHTSPEED-ADMINS"], { admin: true }]) {
      expect(applicationAccess({ groups }, identityEnv.oidc!)).toEqual({ admitted: false, admin: false });
    }
  });
});
