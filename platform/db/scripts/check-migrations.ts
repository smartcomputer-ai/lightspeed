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
  const journal = await readJournal();
  if (journal.entries.length !== schemaRevision) {
    throw new Error("platform schema revision does not match the migration journal");
  }
  if (journal.entries.length === 1) {
    // A freshly rebased ledger has no earlier baseline to upgrade from; the
    // empty-install check is the whole gate until the next migration lands.
    if (journal.entries[0]?.tag !== upgradeFrom) {
      throw new Error("platform upgrade baseline must be the single ledger entry");
    }
    console.log("platform migrations: single baseline; upgrade check starts with the next migration");
  } else {
    await preparePreviousMigrations(previousMigrations, journal);
    await checkUpgrade(databaseUrl(baseUrl, upgradeName), previousMigrations, journal);
  }
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
    await requirePlatformShape(handle.pool);
  } finally {
    await handle.pool.end();
  }
}

/// The platform database holds people and the universe mapping, nothing else:
/// better-auth tables plus universes and setup installations. Bots, triggers,
/// events, channel accounts, and pairings are core tables in the Rust
/// runtime's schema (crates/store-pg) and must never reappear here.
async function requirePlatformShape(pool: pg.Pool): Promise<void> {
  await requireTable(pool, "universes");
  await requireTable(pool, "universe_setup_installations");
  await requireTable(pool, "user");
  await requireTable(pool, "organization");
  await requireTable(pool, "member");
  await requireColumn(pool, "universes", "lightspeed_universe_id");
  for (const table of [
    "bots",
    "bot_triggers",
    "bot_events",
    "channel_accounts",
    "channel_identities",
    "channel_pairings",
    "channel_bindings",
    "bot_activity",
  ]) {
    await requireNoTable(pool, table);
  }
}

interface Journal {
  entries: Array<{ tag?: string }>;
}

async function readJournal(): Promise<Journal> {
  const journalFile = path.join(migrationsFolder, "meta", "_journal.json");
  const journal = JSON.parse(await readFile(journalFile, "utf8")) as { entries?: unknown };
  if (!Array.isArray(journal.entries) || journal.entries.length === 0) {
    throw new Error("migration journal has no entries");
  }
  return { entries: journal.entries as Journal["entries"] };
}

/// Upgrade check, generic over what the migrations contain: the baseline
/// ledger applies cleanly, then the remaining migrations apply on top and
/// the ledger ends at the current head. Table assertions stay for the
/// load-bearing tables; column-level drift belongs to the migrations.
async function checkUpgrade(
  connectionString: string,
  previousFolder: string,
  journal: Journal,
): Promise<void> {
  const handle = createDb(connectionString);
  try {
    await migrate(handle.db, { migrationsFolder: previousFolder });
    await requireTable(handle.pool, "universes");
    const baselineIndex = journal.entries.findIndex((entry) => entry.tag === upgradeFrom);
    await requireLedgerLength(handle.pool, baselineIndex + 1);
    await migrateDb(handle);
    await requirePlatformShape(handle.pool);
    await requireLedgerLength(handle.pool, journal.entries.length);
  } finally {
    await handle.pool.end();
  }
}

async function requireLedgerLength(pool: pg.Pool, expected: number): Promise<void> {
  const result = await pool.query<{ count: string }>(
    "select count(*)::text as count from drizzle.__drizzle_migrations",
  );
  const actual = Number(result.rows[0]?.count ?? "0");
  if (actual !== expected) {
    throw new Error(`migration ledger has ${actual} entries, expected ${expected}`);
  }
}

async function preparePreviousMigrations(destination: string, journal: Journal): Promise<void> {
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

/// information_schema instead of to_regclass: reserved identifiers
/// (better-auth's "user") trip regclass parsing.
async function tableExists(pool: pg.Pool, table: string): Promise<boolean> {
  const result = await pool.query<{ present: boolean }>(
    `select exists (
       select 1 from information_schema.tables
       where table_schema = 'public' and table_name = $1
     ) as present`,
    [table],
  );
  return result.rows[0]?.present === true;
}

async function requireTable(pool: pg.Pool, table: string): Promise<void> {
  if (!(await tableExists(pool, table))) {
    throw new Error(`migration did not create public.${table}`);
  }
}

async function requireNoTable(pool: pg.Pool, table: string): Promise<void> {
  if (await tableExists(pool, table)) {
    throw new Error(`migration left public.${table} in place`);
  }
}

async function requireColumn(pool: pg.Pool, table: string, column: string): Promise<void> {
  const result = await pool.query<{ present: boolean }>(
    `select exists (
       select 1
       from information_schema.columns
       where table_schema = 'public' and table_name = $1 and column_name = $2
     ) as present`,
    [table, column],
  );
  if (result.rows[0]?.present !== true) {
    throw new Error(`migration did not create public.${table}.${column}`);
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
