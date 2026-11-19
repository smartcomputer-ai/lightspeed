import { afterAll, beforeAll, expect, it } from "vitest";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { identityEnv, identityFixture, responseCookies } from "../test/identity-fixture.js";
import { workloadIssuer } from "../test/workload-issuer.js";

let issuer: Awaited<ReturnType<typeof workloadIssuer>>;
let fixture: Awaited<ReturnType<typeof identityFixture>>;
let cookie: string;
beforeAll(async () => {
  issuer = await workloadIssuer();
  fixture = await identityFixture({ ...identityEnv, federation: issuer.federation });
  cookie = responseCookies(
    await fixture.request("/api/auth/sign-in/email", {
      email: identityEnv.adminEmail,
      password: identityEnv.adminPassword,
    }),
  );
}, 30_000);
afterAll(async () => {
  await fixture?.postgres.close();
  await issuer?.close();
});

it("manages bindings within an organization and includes the non-secret declaration in its directory", async () => {
  const id = crypto.randomUUID();
  await fixture.db
    .insert(schema.organization)
    .values({ id, slug: id, name: "Research", createdAt: new Date() });
  const [admin, reader] = await fixture.db
    .insert(schema.serviceUsers)
    .values([
      { organizationId: id, name: "Admin", role: "admin" },
      { organizationId: id, name: "Reader", role: "member" },
    ])
    .returning();
  const directory = `/api/v1/platform/organizations/${id}/service-users`;
  const path = `${directory}/${admin!.id}/federated-credentials`;
  const readerPath = `${directory}/${reader!.id}/federated-credentials`;
  const identity = { issuer: "cluster", subject: issuer.subject };
  const put = (at: string, input: object) => fixture.request(at, input, cookie, "PUT");
  expect((await put(path, { ...identity, issuer: "absent" })).status).toBe(400);
  expect((await put(path, { ...identity, serviceAccountUid: "bad\u0000uid" })).status).toBe(400);
  const initial = await put(path, identity);
  expect(initial.status).toBe(200);
  const { credential } = (await initial.json()) as { credential: { id: string } };
  expect(await (await put(path, identity)).json()).toMatchObject({ credential: { id: credential.id } });
  expect((await put(path, { ...identity, createOnly: true })).status).toBe(409);
  expect((await put(readerPath, identity)).status).toBe(409);
  const listed = await fixture.request(path, undefined, cookie);
  expect(await listed.json()).toMatchObject({
    issuers: ["cluster"],
    credentials: [{ ...identity, id: credential.id, lastUsedAt: null }],
  });
  expect(await (await fixture.request(directory, undefined, cookie)).json()).toMatchObject({
    serviceUsers: expect.arrayContaining([
      expect.objectContaining({
        name: "Admin",
        federatedCredentials: [{ ...identity, restrictions: {}, serviceAccountUid: null }],
      }),
    ]),
  });
  const token = await issuer.mint();
  const headers = { authorization: `Bearer ${token}`, "content-type": "application/json" };
  const delegate = (input: object) =>
    fixture.app.request(readerPath, { method: "PUT", headers, body: JSON.stringify(input) });
  const other = { issuer: "cluster", subject: "other" };
  expect((await delegate(other)).status).toBe(403);
  const inventory = await fixture.request(path, undefined, cookie);
  expect(await inventory.json()).toMatchObject({
    credentials: [{ id: credential.id, lastUsedAt: expect.any(String) }],
  });
  const operation = "platform/service-users/federated-credentials/put";
  const limits = { allowServiceUserKeys: true, operations: [operation] };
  expect((await put(path, { ...identity, restrictions: limits })).status).toBe(200);
  expect((await delegate(other)).status).toBe(403);
  expect((await delegate({ ...other, restrictions: { operations: [operation] } })).status).toBe(200);
  expect((await fixture.request(readerPath + "/" + credential.id, undefined, cookie, "DELETE")).status).toBe(
    404,
  );
  expect((await fixture.request(path + "/" + credential.id, undefined, cookie, "DELETE")).status).toBe(200);
  expect((await fixture.app.request("/api/v1/platform/session", { headers })).status).toBe(401);
  const events = await fixture.db
    .select()
    .from(schema.identityAudit)
    .where(eq(schema.identityAudit.targetId, credential.id));
  expect(events.map((event) => event.action).sort()).toEqual([
    "federatedCredential.delete",
    "federatedCredential.put",
    "federatedCredential.put",
  ]);
});
