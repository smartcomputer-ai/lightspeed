// Fails when a locked npm package version is younger than the minimum age.
//
// A compromised release is usually found and withdrawn within days of being
// published, so a version that has been public for a week is far less likely
// to carry one. Lockfiles pin exact versions; this check reads the publish
// time of every locked version from the npm registry.
//
// A fix that cannot wait is listed in package-age-exceptions.json as
//   { "package": "<name>@<version>", "reason": "<why it cannot wait>" }
// An entry covers that exact version only, and stops mattering once the
// version is old enough.
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';

export const MIN_AGE_DAYS = 7;
export const EXCEPTIONS_PATH = 'scripts/ci/package-age-exceptions.json';

const DAY_MS = 86_400_000;
const REGISTRY = 'https://registry.npmjs.org/';
const INSTALL_DIR = 'node_modules/';

/** Third-party packages a parsed lockfile installs, as `{ name, version, registry }`. */
export function lockedPackages(lock) {
  if (!lock.packages) throw new Error('lockfile has no "packages" map; lockfileVersion 2 or later is required');
  const packages = [];
  for (const [path, entry] of Object.entries(lock.packages)) {
    // The root, workspace sources and links to them are first-party. A bundled
    // copy ships inside its parent's tarball, which is checked itself.
    const at = path.lastIndexOf(INSTALL_DIR);
    if (at === -1 || entry.link || entry.inBundle) continue;
    packages.push({
      name: entry.name ?? path.slice(at + INSTALL_DIR.length),
      version: entry.version,
      registry: entry.resolved?.startsWith(REGISTRY) ?? false,
    });
  }
  return packages;
}

/** Validates the parsed exceptions file and returns a map from `name@version` to its reason. */
export function parseExceptions(value) {
  if (!Array.isArray(value)) throw new Error('package age exceptions must be a JSON array');
  const exceptions = new Map();
  for (const entry of value) {
    const described = JSON.stringify(entry);
    if (typeof entry?.package !== 'string' || !/^(@[^/@\s]+\/)?[^/@\s]+@[^@\s]+$/.test(entry.package)) {
      throw new Error(`package age exception ${described} needs "package" as an exact <name>@<version>`);
    }
    if (typeof entry.reason !== 'string' || !entry.reason.trim()) {
      throw new Error(`package age exception ${described} needs a "reason"`);
    }
    exceptions.set(entry.package, entry.reason.trim());
  }
  return exceptions;
}

/** Publish times of every version of one package, as the registry reports them. */
export async function fetchPublishTimes(name, { fetch = globalThis.fetch, attempts = 4, delayMs = 500 } = {}) {
  const url = REGISTRY + name.replace('/', '%2F');
  for (let attempt = 1; ; attempt += 1) {
    let failure;
    let retry = true;
    try {
      const response = await fetch(url, { headers: { accept: 'application/json' } });
      if (response.ok) return (await response.json()).time ?? {};
      if (response.status === 404) return {};
      failure = `HTTP ${response.status}`;
      retry = response.status === 429 || response.status >= 500;
    } catch (error) {
      failure = error.message;
    }
    if (!retry || attempt >= attempts) throw new Error(`could not read ${name} from the npm registry: ${failure}`);
    await sleep(delayMs * 2 ** (attempt - 1));
  }
}

/**
 * Checks every locked version in `lockfiles` (path to parsed lockfile).
 * `publishTimes(name)` resolves to that package's version-to-time map.
 */
export async function checkPackageAges({
  lockfiles, exceptions = new Map(), publishTimes, now = Date.now(), minAgeDays = MIN_AGE_DAYS, concurrency = 16,
}) {
  const locked = new Map();
  for (const [file, lock] of Object.entries(lockfiles)) {
    for (const { name, version, registry } of lockedPackages(lock)) {
      const id = `${name}@${version}`;
      const seen = locked.get(id) ?? { id, name, version, registry: true, lockfiles: new Set() };
      seen.registry &&= registry;
      seen.lockfiles.add(file);
      locked.set(id, seen);
    }
  }

  const times = new Map();
  const names = [...new Set([...locked.values()].filter((pkg) => pkg.registry).map((pkg) => pkg.name))];
  await Promise.all(Array.from({ length: concurrency }, async () => {
    for (let name; (name = names.pop()) !== undefined;) times.set(name, await publishTimes(name));
  }));

  const result = { checked: locked.size, minAgeDays, violations: [], excepted: [], unused: [] };
  for (const pkg of [...locked.values()].sort((a, b) => a.id.localeCompare(b.id))) {
    const problem = ageProblem(pkg, times.get(pkg.name), now, minAgeDays);
    if (!problem) continue;
    const finding = { package: pkg.id, problem, lockfiles: [...pkg.lockfiles].sort() };
    if (exceptions.has(pkg.id)) result.excepted.push({ ...finding, reason: exceptions.get(pkg.id) });
    else result.violations.push(finding);
  }
  const used = new Set(result.excepted.map((finding) => finding.package));
  result.unused = [...exceptions.keys()].filter((id) => !used.has(id)).sort();
  return result;
}

function ageProblem(pkg, times, now, minAgeDays) {
  if (!pkg.registry) return 'is not installed from the npm registry, so its age cannot be checked';
  const published = Date.parse(times?.[pkg.version]);
  if (Number.isNaN(published)) return 'has no publish time in the npm registry';
  const allowedFrom = published + minAgeDays * DAY_MS;
  if (now >= allowedFrom) return null;
  return `was published ${timestamp(published)} and is allowed from ${timestamp(allowedFrom)}`;
}

function timestamp(ms) {
  return `${new Date(ms).toISOString().slice(0, 16).replace('T', ' ')} UTC`;
}

/** Human-readable outcome of `checkPackageAges`. */
export function formatReport({ checked, minAgeDays, violations, excepted, unused }) {
  const lines = [];
  for (const { package: id, problem, reason } of excepted) lines.push(`Excepted: ${id} ${problem}. Reason: ${reason}`);
  for (const id of unused) lines.push(`Exception no longer needed: ${id}. Remove it from ${EXCEPTIONS_PATH}.`);
  if (violations.length === 0) {
    lines.push(`Package age: ${checked} locked npm versions checked; none is younger than ${minAgeDays} days.`);
    return { ok: true, lines };
  }
  lines.push(`Package age: ${violations.length} of ${checked} locked npm versions fail the ${minAgeDays}-day minimum age:`);
  for (const { package: id, problem, lockfiles } of violations) lines.push(`  ${id} ${problem} (${lockfiles.join(', ')})`);
  lines.push(
    'Lock an older version, or wait until the version is old enough.',
    `For a fix that cannot wait, add { "package": "<name>@<version>", "reason": "..." } to ${EXCEPTIONS_PATH}.`,
  );
  return { ok: false, lines };
}

if (import.meta.main) {
  const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
  const paths = execFileSync('git', ['ls-files', '-z', '--', 'package-lock.json', '*/package-lock.json'], {
    cwd: root, encoding: 'utf8',
  }).split('\0').filter(Boolean);
  if (paths.length === 0) throw new Error('found no tracked package-lock.json to check');
  const readJson = (path) => JSON.parse(readFileSync(join(root, path), 'utf8'));

  const { ok, lines } = formatReport(await checkPackageAges({
    lockfiles: Object.fromEntries(paths.map((path) => [path, readJson(path)])),
    exceptions: parseExceptions(readJson(EXCEPTIONS_PATH)),
    publishTimes: fetchPublishTimes,
  }));
  (ok ? console.log : console.error)(lines.join('\n'));
  if (!ok) process.exitCode = 1;
}
