import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

export const releasePackages = [
  'release-info', 'temporal-server', 'environment-provider-incus',
  'environment-daemon', 'cli',
];

export function validateVersion(version) {
  // Stable releases only: publication currently assigns npm's latest tag.
  if (typeof version !== 'string' || version !== version.trim()
    || !/^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.test(version)) {
    throw new Error('expected a stable version such as 0.3.0 (no v prefix, prerelease, or build metadata)');
  }
}

function replaceOnce(source, pattern, replacement, file) {
  const matches = [...source.matchAll(new RegExp(pattern.source, 'gm'))];
  if (matches.length !== 1) throw new Error(`${file}: expected exactly one product version`);
  return source.replace(pattern, replacement);
}

export function verifyVersions(cwd) {
  const metadata = readFileSync(join(cwd, 'release/metadata.env'), 'utf8');
  const version = metadata.match(/^LIGHTSPEED_PRODUCT_VERSION=(.*)$/m)?.[1];
  validateVersion(version);
  // --no-deps metadata alone does not check whether Cargo.lock is stale.
  execFileSync('cargo', ['update', '--workspace', '--offline', '--locked'], {
    cwd, stdio: 'pipe',
  });
  const cargo = JSON.parse(execFileSync('cargo', [
    'metadata', '--offline', '--no-deps', '--format-version=1', '--locked',
  ], { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], maxBuffer: 16 * 1024 * 1024 }));
  for (const name of releasePackages) {
    const pkg = cargo.packages.find((item) => item.name === name && cargo.workspace_members.includes(item.id));
    if (pkg?.version !== version) {
      throw new Error(`${name}: expected product version ${version}, found ${pkg?.version ?? 'missing package'}`);
    }
    const manifest = readFileSync(pkg.manifest_path, 'utf8');
    if (!/^version\.workspace = true$/m.test(manifest)) {
      throw new Error(`${name}: product version must inherit from workspace.package`);
    }
  }
  return version;
}

export function prepareVersion(cwd, version) {
  validateVersion(version);
  const paths = ['Cargo.toml', 'release/metadata.env', 'Cargo.lock'];
  const originals = new Map(paths.map((path) => [path,
    existsSync(join(cwd, path)) ? readFileSync(join(cwd, path), 'utf8') : null,
  ]));
  // Validate both edit locations before writing anything. Cargo alone updates
  // the lockfile; dependency versions stay locked unless resolution requires it.
  const cargo = replaceOnce(originals.get('Cargo.toml'),
    /^(\[workspace\.package\]\n(?:[^\[]*?\n)?)version = "[^"]+"/m,
    `$1version = "${version}"`, 'Cargo.toml');
  const metadata = replaceOnce(originals.get('release/metadata.env'),
    /^LIGHTSPEED_PRODUCT_VERSION=.*$/m,
    `LIGHTSPEED_PRODUCT_VERSION=${version}`, 'release/metadata.env');
  try {
    writeFileSync(join(cwd, 'Cargo.toml'), cargo);
    writeFileSync(join(cwd, 'release/metadata.env'), metadata);
    execFileSync('cargo', ['update', '--workspace', '--offline'], { cwd, stdio: 'pipe' });
    verifyVersions(cwd);
  } catch (error) {
    for (const [path, contents] of originals) {
      if (contents === null) rmSync(join(cwd, path), { force: true });
      else writeFileSync(join(cwd, path), contents);
    }
    throw error;
  }
}

if (import.meta.main) {
  const cwd = fileURLToPath(new URL('../../', import.meta.url));
  const args = process.argv.slice(2);
  if (args.length !== 1) throw new Error('usage: prepare-version.mjs <0.3.0|--check>');
  if (args[0] === '--check') {
    console.log(`Product version ${verifyVersions(cwd)} is consistent.`);
  } else {
    prepareVersion(cwd, args[0]);
    console.log(`Prepared v${args[0]}. Review the diff, write release/notes/v${args[0]}.md, and run scripts/release/verify-metadata.sh.`);
    console.log('This command does not commit, tag, or publish. Merge the reviewed change into main before tagging.');
  }
}
