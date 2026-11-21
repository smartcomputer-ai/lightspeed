import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import { PGlite } from "@electric-sql/pglite";
import { drizzle } from "drizzle-orm/pglite";
import { afterAll, beforeAll, expect, it } from "vitest";
import type { ModuleDatabase } from "@lightspeed-ai/platform-backend/modules";
import { migrateModuleDatabases } from "@lightspeed-ai/platform-backend/server";
import { ARTIFACT_MAX_BYTES } from "../contract.js";
import { artifacts } from "../module.js";
import { getArtifact, getArtifactContent, listArtifacts, publishArtifact, retractArtifact } from "./index.js";

const selected = { ...artifacts, migrationsFolder: fileURLToPath(new URL("./migrations", import.meta.url)) };
const postgres = new PGlite();
const db = drizzle(postgres) as unknown as ModuleDatabase;
const snapshot = {
  title: "Sample", workspaceId: "workspace", path: "sample.bin", blobRef: "source-digest",
  mediaType: null, sizeBytes: 4, content: Uint8Array.from([0, 127, 128, 255]),
};

beforeAll(async () => {
  await postgres.exec('CREATE TABLE organization (id text PRIMARY KEY); CREATE TABLE universes (id uuid PRIMARY KEY)');
  await migrateModuleDatabases(db, [selected]);
});
afterAll(() => postgres.close());

async function scope(organizationId = randomUUID()) {
  const universeId = randomUUID();
  await postgres.query("INSERT INTO organization VALUES ($1) ON CONFLICT DO NOTHING", [organizationId]);
  await postgres.query("INSERT INTO universes VALUES ($1)", [universeId]);
  return { organizationId, universeId, actorId: "publisher" };
}

it("runs only selected migrations, preserves data on restart and detects a divergent ledger", async () => {
  const owner = await scope();
  const item = await publishArtifact(db, owner, snapshot);
  await migrateModuleDatabases(db, []);
  await migrateModuleDatabases(db, [selected]);
  expect(await getArtifact(db, owner, item.id)).toEqual(item);
  expect((await postgres.query("SELECT version FROM drizzle.module_artifacts")).rows).toEqual([{ version: 0 }]);

  const probe = new PGlite();
  try {
    const connection = drizzle(probe) as unknown as ModuleDatabase;
    await migrateModuleDatabases(connection, []);
    expect((await probe.query("SELECT to_regnamespace('artifacts') AS installed")).rows).toEqual([{ installed: null }]);
    // A missing Platform owner table rolls back the module's DDL and ledger together.
    await expect(migrateModuleDatabases(connection, [selected])).rejects.toThrow();
    expect((await probe.query("SELECT to_regnamespace('artifacts') AS installed")).rows).toEqual([{ installed: null }]);
    await probe.exec("CREATE TABLE organization (id text PRIMARY KEY); CREATE TABLE universes (id uuid PRIMARY KEY)");
    await migrateModuleDatabases(connection, [selected]);
    await probe.exec("UPDATE drizzle.module_artifacts SET hash = 'edited'");
    await expect(migrateModuleDatabases(connection, [selected])).rejects.toThrow("does not match the installed history");
  } finally {
    await probe.close();
  }
}, 15000);

it("scopes immutable snapshots to both the organization and universe and returns only metadata for the shelf", async () => {
  const owner = await scope();
  const elsewhere = await scope(owner.organizationId);
  const item = await publishArtifact(db, owner, snapshot);
  expect(item).not.toHaveProperty("content");
  expect(item.createdBy).toBe(owner.actorId);
  expect(await listArtifacts(db, owner, snapshot.workspaceId)).toEqual([item]);
  expect(await listArtifacts(db, owner, "another-workspace")).toEqual([]);
  expect((await getArtifactContent(db, owner, item.id))?.content).toEqual(snapshot.content);
  for (const other of [elsewhere, { ...owner, organizationId: randomUUID() }]) {
    expect(await listArtifacts(db, other)).toEqual([]);
    expect(await getArtifact(db, other, item.id)).toBeUndefined();
    expect(await getArtifactContent(db, other, item.id)).toBeUndefined();
    expect(await retractArtifact(db, other, item.id)).toBe(false);
  }
  expect(await retractArtifact(db, owner, item.id)).toBe(true);
  expect(await getArtifactContent(db, owner, item.id)).toBeUndefined();
});

it("rejects inconsistent and oversized content at the database boundary", async () => {
  const owner = await scope();
  await expect(publishArtifact(db, owner, { ...snapshot, sizeBytes: 3 })).rejects.toThrow();
  const content = new Uint8Array(ARTIFACT_MAX_BYTES + 1);
  await expect(publishArtifact(db, owner, { ...snapshot, sizeBytes: content.length, content })).rejects.toThrow();
  expect(await listArtifacts(db, owner)).toEqual([]);
});

it("deletes snapshots when their organization or universe is deleted", async () => {
  for (const table of ["organization", "universes"]) {
    const owner = await scope();
    const item = await publishArtifact(db, owner, snapshot);
    await postgres.query(`DELETE FROM ${table} WHERE id = $1`, [table === "organization" ? owner.organizationId : owner.universeId]);
    expect(await getArtifactContent(db, owner, item.id)).toBeUndefined();
  }
});
