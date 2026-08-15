import { cp, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { migrate } from "drizzle-orm/node-postgres/migrator";
import pg from "pg";
import { createDb, migrateDb } from "../src/index.js";

const baseUrl = process.env.LIGHTSPEED_PLATFORM_MIGRATION_TEST_URL;
if (!baseUrl) {
  throw new Error("LIGHTSPEED_PLATFORM_MIGRATION_TEST_URL is required");
}

const migrationsFolder = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "migrations",
);
const releaseMetadata = parseMetadata(
  await readFile(
    path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "release", "metadata.env"),
    "utf8",
  ),
);
const upgradeFrom = releaseMetadata.LIGHTSPEED_PLATFORM_UPGRADE_FROM;
const schemaRevision = Number(releaseMetadata.LIGHTSPEED_PLATFORM_SCHEMA_REVISION);
if (!upgradeFrom || !Number.isSafeInteger(schemaRevision) || schemaRevision < 1) {
  throw new Error("release metadata has invalid platform migration compatibility");
}
const suffix = `${process.pid}_${Date.now()}`;
const emptyName = checkedIdentifier(`lightspeed_platform_empty_${suffix}`);
const upgradeName = checkedIdentifier(`lightspeed_platform_upgrade_${suffix}`);
const admin = new pg.Client({ connectionString: baseUrl });
const previousMigrations = await mkdtemp(path.join(tmpdir(), "lightspeed-platform-migrations-"));
let adminConnected = false;

try {
  await admin.connect();
  adminConnected = true;
  await createDatabase(admin, emptyName);
  await createDatabase(admin, upgradeName);

  await checkEmptyInstall(databaseUrl(baseUrl, emptyName));
  await preparePreviousMigrations(previousMigrations);
  await checkUpgrade(databaseUrl(baseUrl, upgradeName), previousMigrations);
} finally {
  if (adminConnected) {
    await dropDatabase(admin, emptyName);
    await dropDatabase(admin, upgradeName);
  }
  await admin.end().catch(() => undefined);
  await rm(previousMigrations, { recursive: true, force: true });
}

async function checkEmptyInstall(connectionString: string): Promise<void> {
  const handle = createDb(connectionString);
  try {
    await migrateDb(handle);
    await requireTable(handle.pool, "universes");
    await requireTable(handle.pool, "foundry_releases");
  } finally {
    await handle.pool.end();
  }
}

async function checkUpgrade(
  connectionString: string,
  previousFolder: string,
): Promise<void> {
  const handle = createDb(connectionString);
  try {
    await migrate(handle.db, { migrationsFolder: previousFolder });
    await requireTable(handle.pool, "universes");
    await requireMissingTable(handle.pool, "foundry_releases");
    await migrateDb(handle);
    await requireTable(handle.pool, "foundry_releases");
  } finally {
    await handle.pool.end();
  }
}

async function preparePreviousMigrations(destination: string): Promise<void> {
  const journalFile = path.join(migrationsFolder, "meta", "_journal.json");
  const journal = JSON.parse(await readFile(journalFile, "utf8")) as {
    entries?: Array<{ tag?: string }>;
  };
  if (!Array.isArray(journal.entries) || journal.entries.length < 2) {
    throw new Error("migration upgrade check requires at least two ledger entries");
  }
  if (journal.entries.length !== schemaRevision) {
    throw new Error("platform schema revision does not match the migration journal");
  }
  const baselineIndex = journal.entries.findIndex((entry) => entry.tag === upgradeFrom);
  if (baselineIndex < 0 || baselineIndex === journal.entries.length - 1) {
    throw new Error("platform upgrade baseline must exist before the current migration");
  }
  const previousEntries = journal.entries.slice(0, baselineIndex + 1);
  await cp(path.join(migrationsFolder, "meta"), path.join(destination, "meta"), {
    recursive: true,
  });
  await writeFile(
    path.join(destination, "meta", "_journal.json"),
    `${JSON.stringify({ ...journal, entries: previousEntries }, null, 2)}\n`,
  );
  for (const entry of previousEntries) {
    if (!entry.tag) throw new Error("migration journal entry is missing its tag");
    await cp(
      path.join(migrationsFolder, `${entry.tag}.sql`),
      path.join(destination, `${entry.tag}.sql`),
    );
  }
}

async function requireTable(pool: pg.Pool, table: string): Promise<void> {
  const result = await pool.query<{ relation: string | null }>(
    "select to_regclass($1) as relation",
    [`public.${table}`],
  );
  if (result.rows[0]?.relation !== table) {
    throw new Error(`migration did not create public.${table}`);
  }
}

async function requireMissingTable(pool: pg.Pool, table: string): Promise<void> {
  const result = await pool.query<{ relation: string | null }>(
    "select to_regclass($1) as relation",
    [`public.${table}`],
  );
  if (result.rows[0]?.relation !== null) {
    throw new Error(`previous-release migration fixture unexpectedly contains public.${table}`);
  }
}

async function createDatabase(client: pg.Client, name: string): Promise<void> {
  await client.query(`CREATE DATABASE "${name}"`);
}

async function dropDatabase(client: pg.Client, name: string): Promise<void> {
  await client.query(`DROP DATABASE IF EXISTS "${name}" WITH (FORCE)`);
}

function databaseUrl(value: string, database: string): string {
  const url = new URL(value);
  url.pathname = `/${database}`;
  return url.toString();
}

function checkedIdentifier(value: string): string {
  if (!/^[a-z0-9_]+$/.test(value)) throw new Error("unsafe generated database name");
  return value;
}

function parseMetadata(value: string): Record<string, string> {
  return Object.fromEntries(
    value
      .split("\n")
      .filter((line) => line.length > 0 && !line.startsWith("#"))
      .map((line) => line.split(/=(.*)/s).slice(0, 2) as [string, string]),
  );
}
