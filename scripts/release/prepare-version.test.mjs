import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { prepareVersion, releasePackages, validateVersion, verifyVersions } from './prepare-version.mjs';

function fixture(t) {
  const cwd = mkdtempSync(join(tmpdir(), 'lightspeed-release-version-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const write = (path, text) => {
    mkdirSync(dirname(join(cwd, path)), { recursive: true });
    writeFileSync(join(cwd, path), text);
  };
  const read = (path) => readFileSync(join(cwd, path), 'utf8');
  write('Cargo.toml', `[workspace]\nmembers = [${releasePackages.map((name) => `"crates/${name}"`).join(', ')}]\nresolver = "2"\n\n[workspace.package]\nversion = "0.2.0"\n`);
  write('release/metadata.env', 'LIGHTSPEED_PRODUCT_VERSION=0.2.0\nLIGHTSPEED_SCHEMA_REVISION=10\n');
  for (const name of releasePackages) {
    write(`crates/${name}/Cargo.toml`, `[package]\nname = "${name}"\nversion.workspace = true\nedition = "2024"\n`);
    write(`crates/${name}/src/lib.rs`, '');
  }
  execFileSync('cargo', ['generate-lockfile', '--offline'], { cwd, stdio: 'pipe' });
  return { cwd, write, read };
}

test('preparation updates every released package and lockfile without changing compatibility metadata', (t) => {
  const f = fixture(t);
  prepareVersion(f.cwd, '0.3.0');
  assert.equal(verifyVersions(f.cwd), '0.3.0');
  assert.equal(f.read('release/metadata.env'), 'LIGHTSPEED_PRODUCT_VERSION=0.3.0\nLIGHTSPEED_SCHEMA_REVISION=10\n');
  assert.equal((f.read('Cargo.lock').match(/version = "0.3.0"/g) ?? []).length, releasePackages.length);
  const lock = f.read('Cargo.lock');
  prepareVersion(f.cwd, '0.3.0');
  assert.equal(f.read('Cargo.lock'), lock);
});

test('invalid versions and unsupported prerelease channels are rejected', () => {
  for (const version of [undefined, '', 'v0.3.0', '0.3', '00.3.0', '0.03.0', '0.3.00', '0.3.0-rc.1', '0.3.0+build', '0.3.0\nextra', '0.3.0\n']) {
    assert.throws(() => validateVersion(version), /expected a stable version/);
  }
  for (const version of ['0.0.0', '0.3.0', '1.20.300']) validateVersion(version);
});

test('verification detects a stale lockfile without modifying it', (t) => {
  const f = fixture(t);
  const lock = f.read('Cargo.lock');
  f.write('Cargo.toml', f.read('Cargo.toml').replace('0.2.0', '0.3.0'));
  f.write('release/metadata.env', f.read('release/metadata.env').replace('0.2.0', '0.3.0'));
  assert.throws(() => verifyVersions(f.cwd));
  assert.equal(f.read('Cargo.lock'), lock);
});

test('failed preparation restores manifests and lockfile, preserving existing edits', (t) => {
  const f = fixture(t);
  const manifest = 'crates/cli/Cargo.toml';
  f.write(manifest, f.read(manifest).replace('version.workspace = true', 'version = "0.2.0"'));
  f.write('release/metadata.env', `${f.read('release/metadata.env')}# local note\n`);
  const paths = ['Cargo.toml', 'release/metadata.env', 'Cargo.lock', manifest];
  const before = paths.map(f.read);
  assert.throws(() => prepareVersion(f.cwd, '0.3.0'), /cli: expected product version/);
  assert.deepEqual(paths.map(f.read), before);
});

test('duplicate metadata is rejected before modifying files', (t) => {
  const f = fixture(t);
  f.write('release/metadata.env', `${f.read('release/metadata.env')}LIGHTSPEED_PRODUCT_VERSION=0.2.0\n`);
  const before = f.read('Cargo.toml');
  assert.throws(() => prepareVersion(f.cwd, '0.3.0'), /expected exactly one product version/);
  assert.equal(f.read('Cargo.toml'), before);
});
