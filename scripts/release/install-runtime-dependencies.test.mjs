import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

const installScript = fileURLToPath(new URL('./install-runtime-dependencies.mjs', import.meta.url));

test('runtime install excludes optional development peers and retains locked production, required-peer and native optional dependencies', (t) => {
  const root = fs.mkdtempSync(path.join(tmpdir(), 'lightspeed-runtime-deps-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const writeJson = (file, value) => fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
  const pack = (name, metadata = {}) => {
    const directory = path.join(root, name, 'package');
    fs.mkdirSync(directory, { recursive: true });
    writeJson(path.join(directory, 'package.json'), {
      name, version: '1.0.0', main: './index.js', ...metadata,
    });
    fs.writeFileSync(path.join(directory, 'index.js'), `module.exports = ${JSON.stringify(name)};\n`);
    const archive = path.join(root, `${name}.tgz`);
    execFileSync('tar', ['-czf', archive, '-C', path.dirname(directory), 'package']);
    return `file:${archive}`;
  };
  const production = pack('fixture-production');
  const native = pack('fixture-native', { os: [process.platform], cpu: [process.arch] });
  const requiredPeer = pack('fixture-required-peer');
  const devChild = pack('fixture-dev-child');
  const runner = pack('fixture-runner', { dependencies: { 'fixture-dev-child': devChild } });
  const auth = pack('fixture-auth', {
    dependencies: { 'fixture-production': production },
    optionalDependencies: { 'fixture-native': native },
    peerDependencies: { 'fixture-runner': '1.0.0', 'fixture-required-peer': '1.0.0' },
    peerDependenciesMeta: { 'fixture-runner': { optional: true } },
  });
  const staging = path.join(root, 'staging');
  fs.mkdirSync(path.join(staging, 'app'), { recursive: true });
  writeJson(path.join(staging, 'package.json'), {
    name: 'fixture-root', private: true, workspaces: ['app'],
    devDependencies: { 'fixture-runner': runner, 'fixture-required-peer': requiredPeer },
  });
  writeJson(path.join(staging, 'app/package.json'), {
    name: 'fixture-app', version: '1.0.0', dependencies: { 'fixture-auth': auth },
    devDependencies: { 'fixture-runner': runner },
  });
  const options = {
    cwd: staging, encoding: 'utf8', stdio: 'pipe',
    env: { ...process.env, npm_config_cache: path.join(root, 'cache'), npm_config_offline: 'true' },
  };
  const npm = (args) => execFileSync(process.platform === 'win32' ? 'bash' : 'npm',
    process.platform === 'win32' ? ['-c', 'exec npm "$@"', 'npm', ...args] : args, options);
  const flags = ['--offline', '--ignore-scripts', '--no-audit', '--no-fund'];
  npm(['install', '--package-lock-only', ...flags]);
  const lockFile = path.join(staging, 'package-lock.json');
  const originalLock = JSON.parse(fs.readFileSync(lockFile, 'utf8'));
  npm(['ci', '--workspace', 'fixture-app', '--omit=dev', ...flags]);
  const installed = (name) => fs.existsSync(path.join(staging, 'node_modules', name, 'package.json'));
  assert.ok(installed('fixture-runner'), 'fixture must reproduce npm retaining the optional development peer');

  execFileSync(process.execPath, [installScript, staging, 'fixture-app'], options);
  assert.ok(!installed('fixture-runner'));
  assert.ok(!installed('fixture-dev-child'));
  for (const name of ['fixture-auth', 'fixture-production', 'fixture-required-peer', 'fixture-native']) {
    assert.ok(installed(name), `${name} must remain in the runtime`);
    const location = `node_modules/${name}`;
    const stagedLock = JSON.parse(fs.readFileSync(lockFile, 'utf8'));
    for (const field of ['version', 'resolved', 'integrity']) {
      assert.equal(stagedLock.packages[location][field], originalLock.packages[location][field]);
    }
    assert.equal(execFileSync(process.execPath, ['-e', `console.log(require(${JSON.stringify(name)}))`], options).trim(), name);
  }
  assert.equal(JSON.parse(fs.readFileSync(path.join(staging, 'app/package.json'), 'utf8')).devDependencies, undefined);

  // A stale manifest must fail rather than silently change released versions.
  const changedAuth = pack('fixture-auth-next', { name: 'fixture-auth', version: '2.0.0' });
  writeJson(path.join(staging, 'app/package.json'), {
    name: 'fixture-app', version: '1.0.0', dependencies: { 'fixture-auth': changedAuth },
  });
  assert.throws(() => execFileSync(process.execPath, [installScript, staging, 'fixture-app'], options),
    /Runtime staging changed locked version for node_modules\/fixture-auth/);
});
