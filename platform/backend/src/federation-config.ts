import { readFileSync } from "node:fs";
import { parse } from "yaml";
import { z } from "zod";
import { organizationCreateSchema, serviceUserCreateSchema } from "@lightspeed-ai/platform-shared";

const nonempty = z.string().min(1);
export const keySourceUrl = z.string().url().refine(value => {
  const url = new URL(value);
  return !url.username && !url.password && !url.hash && !url.search &&
    (url.protocol === "https:" || (url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname)));
}, "Use HTTPS, or HTTP on loopback");

const sources = z.union([
  z.strictObject({ jwksFile: nonempty }),
  z.strictObject({
    discoveryUrl: keySourceUrl.optional(), jwksUrl: keySourceUrl.optional(),
    caFile: nonempty.optional(), bearerTokenFile: nonempty.optional(),
  }),
]);
const issuer = z.strictObject({
  name: nonempty, issuer: keySourceUrl, audiences: z.array(nonempty).min(1),
  algorithms: z.array(z.enum(["RS256", "RS384", "RS512", "PS256", "PS384", "PS512", "ES256", "ES384", "ES512", "EdDSA", "Ed25519"])).min(1).default(["RS256"]),
  keys: sources.default({}), maxTokenLifetimeSeconds: z.int().positive().optional(),
  clockSkewSeconds: z.int().min(0).max(300).default(60),
});
const rule = z.strictObject({
  issuer: nonempty, subjectPattern: nonempty.max(512), maxOrganizations: z.int().nonnegative(),
  handlePattern: nonempty.optional().transform(value => value === undefined ? undefined : new RegExp(value, "u")),
  organization: organizationCreateSchema.pick({ universeQuota: true }).default({}),
  serviceUser: z.strictObject({ name: serviceUserCreateSchema.shape.name, role: z.literal("admin") })
    .default({ name: "deployer", role: "admin" }),
});
export type TrustedIssuer = z.infer<typeof issuer>;
export type EnrollmentRule = z.infer<typeof rule>;
export const federationConfig = z.strictObject({
  issuers: z.array(issuer).min(1), enrollment: z.array(rule).default([]),
}).superRefine(({ issuers, enrollment }, ctx) => {
  const names = new Set(issuers.map(i => i.name));
  if (names.size !== issuers.length || new Set(issuers.map(i => i.issuer)).size !== issuers.length)
    ctx.addIssue({ code: "custom", message: "Trusted issuer names and identifiers must be unique" });
  const patterns = new Set<string>();
  for (const r of enrollment) {
    const key = JSON.stringify([r.issuer, r.subjectPattern]);
    if (!names.has(r.issuer) || patterns.has(key))
      ctx.addIssue({ code: "custom", message: "Enrollment rules must identify a trusted issuer and a unique subject pattern" });
    patterns.add(key);
  }
});

export function readFederationConfig(path: string) {
  return federationConfig.parse(parse(readFileSync(path, "utf8")));
}
