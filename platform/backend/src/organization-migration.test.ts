import { readFile } from "node:fs/promises";
import { PGlite } from "@electric-sql/pglite";
import { expect, it } from "vitest";

it("preserves credential ceilings and nested setup provenance when identity terminology changes", async () => {
  const db = new PGlite();
  try {
    const journal = JSON.parse(await readFile(new URL("../../db/migrations/meta/_journal.json", import.meta.url), "utf8")) as { entries: { tag: string }[] };
    for (const entry of journal.entries) {
      if (entry.tag === "0009_organization_identity_data") break;
      await db.exec(await readFile(new URL(`../../db/migrations/${entry.tag}.sql`, import.meta.url), "utf8"));
    }
    await db.exec(`
      INSERT INTO "user" (id, name, email) VALUES ('person', 'Person', 'person@example.test');
      INSERT INTO identity_audit (action, target_id, outcome) VALUES ('account.development_admission', 'person', 'success');
      INSERT INTO organization (id, name, slug, created_at) VALUES ('org', 'Organization', 'org', now());
      INSERT INTO apikey (id, config_id, reference_id, key, created_at, updated_at)
        VALUES ('key', 'application', 'org', 'hashed-key', now(), now());
      INSERT INTO credential_bindings (key_id, user_id, restrictions, created_by)
        VALUES ('key', 'person', '{"accountIds":[],"universeIds":["universe"],"readOnly":true,"allowUniverseCreation":false,"operations":["platform/accounts/applications/list","platform/applications/credentials/list","profiles/read"]}', 'person');
      INSERT INTO universes (id, organization_id, lightspeed_universe_id, name, handle)
        VALUES ('11111111-1111-4111-8111-111111111111', 'org', '22222222-2222-4222-8222-222222222222', 'Universe', 'universe');
      INSERT INTO universe_setup_installations (universe_id, setup_id, state)
        VALUES ('11111111-1111-4111-8111-111111111111', 'configurator', '{"applicationId":"current","credentialId":"key","previous":{"applicationId":"previous","credentialId":"old-key"}}');
    `);
    await db.exec(await readFile(new URL("../../db/migrations/0009_organization_identity_data.sql", import.meta.url), "utf8"));
    const limits = await db.query<{ restrictions: unknown }>("SELECT restrictions FROM credential_bindings");
    expect(limits.rows[0]?.restrictions).toEqual({ organizationIds: [], universeIds: ["universe"], readOnly: true,
      allowUniverseCreation: false, operations: ["platform/organizations/service-users/list", "platform/service-users/credentials/list", "profiles/read"] });
    const keys = await db.query<{ config_id: string }>("SELECT config_id FROM apikey");
    expect(keys.rows[0]?.config_id).toBe("serviceUser");
    const audit = await db.query<{ action: string }>("SELECT action FROM identity_audit");
    expect(audit.rows[0]?.action).toBe("organization.development_admission");
    const setups = await db.query<{ state: unknown }>("SELECT state FROM universe_setup_installations");
    expect(setups.rows[0]?.state).toEqual({ serviceUserId: "current", credentialId: "key", previous: { serviceUserId: "previous", credentialId: "old-key" } });
  } finally { await db.close(); }
});
