import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync, spawnSync } from 'node:child_process';
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { delimiter, dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

const repository = fileURLToPath(new URL('../../', import.meta.url));
const metadata = Object.fromEntries(readFileSync(join(repository, 'release/metadata.env'), 'utf8')
  .split('\n').filter((line) => line && !line.startsWith('#'))
  .map((line) => line.split(/=(.*)/s).slice(0, 2)));
const version = metadata.LIGHTSPEED_PRODUCT_VERSION;
const gitSha = 'a'.repeat(40);
const digest = `sha256:${'b'.repeat(64)}`;
const registry = 'ghcr.io/example/lightspeed';
const hash = (bytes) => createHash('sha256').update(bytes).digest('hex');
const cleanEnv = Object.fromEntries(Object.entries(process.env)
  .filter(([key]) => !key.startsWith('LIGHTSPEED_') && key !== 'SOURCE_DATE_EPOCH'));

function fixture(t) {
  const cwd = mkdtempSync(join(tmpdir(), 'lightspeed-static-release-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const write = (path, bytes) => {
    mkdirSync(dirname(join(cwd, path)), { recursive: true });
    writeFileSync(join(cwd, path), bytes);
  };
  write('release/metadata.env', readFileSync(join(repository, 'release/metadata.env')));
  write('dist/release-manifest.json', JSON.stringify({ rustVersion: `rustc ${metadata.LIGHTSPEED_RELEASE_RUST_VERSION}` }));
  for (const contract of ['api.schema.json', 'methods.json', 'openrpc.json', 'api-reference.md']) {
    write(`dist/contracts/${contract}`, `fixture ${contract}`);
  }
  for (const component of ['runtime', 'provider-incus', 'cli', 'demo']) {
    write(`dist/archives/lightspeed-${component}-${version}.tar.gz`, `fixture ${component}`);
  }
  write(`dist/archives/lightspeed-envd-${version}-${metadata.LIGHTSPEED_ENVD_TARGET}.tar.gz`, 'fixture envd');
  write(`dist/archives/lightspeed-docs-${version}.tar.gz`, 'fixture documentation archive');
  write('dist/npm/client.tgz', 'fixture client');
  const env = {
    ...cleanEnv, LIGHTSPEED_RELEASE_BUILD_IMAGE: `${registry}/build-env@${digest}`,
    SOURCE_DATE_EPOCH: '1700000000',
  };
  for (const name of ['RUNTIME', 'PROVIDER_INCUS', 'ENVD', 'CLI']) {
    env[`LIGHTSPEED_BINARY_URL_${name}`] = `oci://${registry}/${name.toLowerCase().replaceAll('_', '-')}-bundle@${digest}`;
  }
  for (const name of ['DEMO', 'DOCS']) {
    env[`LIGHTSPEED_ARTIFACT_URL_${name}`] = `oci://${registry}/${name.toLowerCase()}-bundle@${digest}`;
  }
  for (const name of ['RUNTIME', 'CONFIGURATOR_MCP', 'PLATFORM', 'PLATFORM_WORKERS']) {
    env[`LIGHTSPEED_${name}_IMAGE`] = `${registry}/${name.toLowerCase().replaceAll('_', '-')}@${digest}`;
  }
  const create = () => execFileSync(process.execPath,
    [join(repository, 'scripts/release/create-manifest.mjs'), version, gitSha], { cwd, env, stdio: 'pipe' });
  const verify = (...args) => spawnSync(process.execPath,
    [join(repository, 'scripts/release/verify-manifest.mjs'), ...args], { cwd, env, encoding: 'utf8' });
  const manifest = () => JSON.parse(readFileSync(join(cwd, 'dist/release-manifest.json'), 'utf8'));
  return { cwd, write, env, create, verify, manifest };
}

for (const channel of ['main', 'release']) {
  test(`${channel} manifests identify the docs archive, checksum, base path, and immutable URL`, (t) => {
    const f = fixture(t);
    f.env.LIGHTSPEED_RELEASE_CHANNEL = channel;
    if (channel === 'release') f.env.LIGHTSPEED_ENVD_PUBLIC_URL_BASE = 'https://example.test/releases/v1';
    f.create();
    const manifest = f.manifest();
    assert.equal(manifest.typescriptClient.name, "@lightspeed-ai/client");
    assert.deepEqual(Object.keys(manifest.binaries).sort(), ['cli', 'envd', 'providerIncus', 'runtime']);
    assert.equal(manifest.binaries.runtime.file, `lightspeed-runtime-${version}.tar.gz`);
    assert.equal(manifest.binaries.runtime.url, `oci://${registry}/runtime-bundle@${digest}`);
    const docs = manifest.artifacts.docs;
    assert.equal(docs.file, `lightspeed-docs-${version}.tar.gz`);
    assert.equal(docs.sha256, hash(readFileSync(join(f.cwd, 'dist/archives', docs.file))));
    assert.equal(docs.basePath, '/docs/');
    assert.equal(docs.url, `oci://${registry}/docs-bundle@${digest}`);
    const result = f.verify('--published');
    assert.equal(result.status, 0, result.stderr);
  });
}

test('local docs archives may be unpublished, but published manifests require an immutable URL', (t) => {
  const f = fixture(t);
  delete f.env.LIGHTSPEED_ARTIFACT_URL_DOCS;
  f.create();
  assert.equal(f.manifest().artifacts.docs.url, null);
  assert.equal(f.verify().status, 0);
  assert.notEqual(f.verify('--published').status, 0);
  f.env.LIGHTSPEED_ARTIFACT_URL_DOCS = `oci://${registry}/docs-bundle:latest`;
  f.create();
  assert.notEqual(f.verify('--published').status, 0);
});

test('manifest verification rejects missing, misplaced, or corrupted documentation', (t) => {
  const f = fixture(t);
  f.create();
  const original = f.manifest();
  for (const change of [
    (manifest) => { delete manifest.artifacts.docs; },
    (manifest) => { manifest.artifacts.docs.basePath = '/'; },
    (manifest) => { manifest.artifacts.docs.sha256 = '0'.repeat(64); },
    (manifest) => { manifest.artifacts.docs.file = '../outside.tar.gz'; },
  ]) {
    const manifest = structuredClone(original);
    change(manifest);
    f.write('dist/release-manifest.json', JSON.stringify(manifest));
    assert.notEqual(f.verify().status, 0);
  }
  f.write('dist/release-manifest.json', JSON.stringify(original));
  f.write(`dist/archives/${original.artifacts.docs.file}`, 'corrupted archive');
  assert.notEqual(f.verify().status, 0);
  rmSync(join(f.cwd, 'dist/archives', original.artifacts.docs.file));
  assert.notEqual(f.verify().status, 0);
  assert.throws(f.create);
});

test('static packaging preserves complete site roots without source files or an extra docs prefix', (t) => {
  const f = fixture(t);
  mkdirSync(join(f.cwd, 'scripts/release'), { recursive: true });
  copyFileSync(join(repository, 'scripts/release/stage-static-sites.sh'),
    join(f.cwd, 'scripts/release/stage-static-sites.sh'));
  const files = ['index.html', 'index.md', 'llms.txt', 'getting-started.md', '404.html', '_astro/site.css', '_astro/font.woff2',
    'pagefind/pagefind.js', 'pagefind/fragment/data.pf_fragment', 'sitemap-index.xml',
    'getting-started/index.html', 'assets/mark.svg', 'licenses/font.txt'];
  for (const file of files) f.write(`docs/site/dist/${file}`, `built ${file}`);
  f.write('docs/site/src/private-source.astro', 'must not be packaged');
  f.write('platform/web/dist-demo/index.html', 'demo');
  const stage = () => execFileSync('bash', ['scripts/release/stage-static-sites.sh'], {
    cwd: f.cwd, env: { ...cleanEnv, LIGHTSPEED_RELEASE_VERSION: version }, stdio: 'pipe',
  });
  stage();
  const archive = join(f.cwd, 'dist/archives', `lightspeed-docs-${version}.tar.gz`);
  const before = hash(readFileSync(archive));
  assert.equal(readFileSync(archive).readUInt32LE(4), 0, 'gzip must omit the build-time timestamp');
  const entries = execFileSync('tar', ['-tzf', archive], { encoding: 'utf8' }).trim().split('\n');
  assert.deepEqual(entries.filter((entry) => !entry.endsWith('/')).sort(), files.map((file) => `./${file}`).sort());
  assert.equal(execFileSync('tar', ['-xOzf', archive, './index.html'], { encoding: 'utf8' }), 'built index.html');
  stage();
  assert.equal(hash(readFileSync(archive)), before, 'identical static files produce identical archives');
  rmSync(join(f.cwd, 'docs/site/dist/index.html'));
  assert.throws(stage, 'missing documentation output must fail the release');
});

test('release aliases include the docs digest; snapshots retain one aggregate identity', (t) => {
  const f = fixture(t);
  f.create();
  mkdirSync(join(f.cwd, 'scripts/release'), { recursive: true });
  for (const script of ['publish-aliases.sh', 'verify-manifest.mjs']) {
    copyFileSync(join(repository, 'scripts/release', script), join(f.cwd, 'scripts/release', script));
  }
  // Local registry doubles exercise publication without network access.
  for (const command of ['oras', 'crane']) {
    f.write(`bin/${command}`, `#!/usr/bin/env node
import { appendFileSync, readFileSync, writeFileSync } from 'node:fs';
const [operation, source, target] = process.argv.slice(2);
const state = JSON.parse(readFileSync(process.env.REGISTRY_TEST_STATE, 'utf8'));
if (operation === 'resolve' || operation === 'digest') {
  const digest = source.includes('@sha256:') ? source.split('@')[1] : state[source];
  if (!digest) process.exit(1);
  console.log(digest);
} else if (operation === 'copy' || operation === 'cp') {
  state[target] = source.split('@')[1];
  writeFileSync(process.env.REGISTRY_TEST_STATE, JSON.stringify(state));
  appendFileSync(process.env.REGISTRY_TEST_LOG, JSON.stringify({ source, target }) + '\\n');
} else {
  throw new Error('Unexpected registry command: ' + operation);
}
`);
    chmodSync(join(f.cwd, 'bin', command), 0o755);
  }
  const env = {
    ...f.env, PATH: `${join(f.cwd, 'bin')}${delimiter}${cleanEnv.PATH}`,
    GHCR_ROOT: registry, REGISTRY_TEST_STATE: join(f.cwd, 'registry-state.json'),
    REGISTRY_TEST_LOG: join(f.cwd, 'registry-copies.jsonl'),
  };
  const reset = () => {
    f.write('registry-state.json', '{}');
    f.write('registry-copies.jsonl', '');
  };
  const publish = (alias) => execFileSync('bash', ['scripts/release/publish-aliases.sh', alias, digest], {
    cwd: f.cwd, env, stdio: 'pipe',
  });
  const copies = () => readFileSync(env.REGISTRY_TEST_LOG, 'utf8').trim().split('\n').map(JSON.parse);
  reset();
  publish(version);
  const releaseCopies = copies();
  assert.ok(releaseCopies.some(({ source, target }) =>
    source === `${registry}/docs-bundle@${digest}` && target === `${registry}/docs-bundle:${version}`));
  assert.equal(releaseCopies.at(-1).target, `${registry}/release-bundle:${version}`);
  publish(version);
  assert.deepEqual(copies(), releaseCopies, 'repeated publication preserves existing aliases');
  reset();
  publish(`sha-${gitSha}`);
  assert.deepEqual(copies(), [{ source: `${registry}/release-bundle@${digest}`, target: `${registry}/release-bundle:sha-${gitSha}` }]);
});
