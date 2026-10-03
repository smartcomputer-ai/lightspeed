import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

const stageScript = fileURLToPath(new URL('./stage-package.mjs', import.meta.url));

test('staged Configurator installs the SDK production graph from its lockfile offline', (t) => {
  const root = mkdtempSync(join(tmpdir(), 'lightspeed-package-test-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const writeJson = (file, data) => writeFileSync(file, JSON.stringify(data));
  const pack = (directory, name, version, dependencies, source) => {
    const packageDir = join(root, directory, 'package');
    mkdirSync(packageDir, { recursive: true });
    writeJson(join(packageDir, 'package.json'), {
      name, version, type: 'module', exports: './index.js', dependencies,
    });
    writeFileSync(join(packageDir, 'index.js'), source);
    const tarball = join(root, `${directory}.tgz`);
    execFileSync('tar', ['-czf', tarball, '-C', join(root, directory), 'package']);
    return {
      version, resolved: `file:${tarball}`,
      integrity: `sha512-${createHash('sha512').update(readFileSync(tarball)).digest('base64')}`,
      dependencies,
    };
  };
  const core = pack('core', 'fixture-core', '1.0.0', {}, 'export default "locked core";');
  const hash = pack('hash', 'fixture-hash', '1.0.0', { 'fixture-core': '1.0.0' },
    'export { default } from "fixture-core";');
  const client = pack('sdk', '@lightspeed-ai/sdk', '0.3.0', { 'fixture-hash': '1.0.0' },
    'export { default } from "fixture-hash";');
  const clientLockPath = join(root, 'client-lock.json');
  writeJson(clientLockPath, {
    name: '@lightspeed-ai/sdk', version: '0.3.0', lockfileVersion: 3,
    packages: {
      '': {
        name: '@lightspeed-ai/sdk', version: '0.3.0',
        dependencies: client.dependencies, devDependencies: { 'fixture-dev': '1.0.0' },
      },
      'node_modules/fixture-hash': hash,
      'node_modules/fixture-core': core,
      'node_modules/fixture-dev': { version: '1.0.0', dev: true },
    },
  });
  const config = join(root, 'configurator');
  mkdirSync(config);
  writeFileSync(join(config, 'sdk.tgz'), readFileSync(join(root, 'sdk.tgz')));
  const manifest = {
    name: '@lightspeed-ai/configurator-mcp', version: '0.0.0', private: true,
    dependencies: { '@lightspeed-ai/sdk': 'file:../old-sdk' },
  };
  writeJson(join(config, 'package.json'), manifest);
  writeJson(join(config, 'package-lock.json'), {
    name: manifest.name, version: manifest.version, lockfileVersion: 3,
    packages: {
      '': manifest,
      '../old-sdk': { name: '@lightspeed-ai/sdk', version: '0.0.0' },
      'node_modules/@lightspeed-ai/sdk': { resolved: '../old-sdk', link: true },
    },
  });
  execFileSync(process.execPath, [stageScript, 'configurator', config, '0.3.0', 'a'.repeat(40), clientLockPath]);
  const lockBeforeInstall = readFileSync(join(config, 'package-lock.json'), 'utf8');
  execFileSync('npm', ['ci', '--omit=dev', '--offline', '--ignore-scripts', '--no-audit', '--cache', join(root, 'cache')],
    { cwd: config, stdio: 'pipe' });
  assert.equal(readFileSync(join(config, 'package-lock.json'), 'utf8'), lockBeforeInstall);
  const result = execFileSync(process.execPath,
    ['--input-type=module', '-e', 'import value from "@lightspeed-ai/sdk"; console.log(value);'],
    { cwd: config, encoding: 'utf8' });
  assert.equal(result.trim(), 'locked core');
  assert.ok(!existsSync(join(config, 'node_modules/@lightspeed-ai/sdk/node_modules/fixture-dev')));
});
