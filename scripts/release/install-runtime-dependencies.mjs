#!/usr/bin/env node
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

// Run only in the disposable staging directory. Optional peers of production
// dependencies can retain development tools even with npm ci --omit=dev.
export function installRuntimeDependencies(directory, workspace) {
  const readJson = (name) => JSON.parse(fs.readFileSync(path.join(directory, name), 'utf8'));
  const writeJson = (name, value) => fs.writeFileSync(path.join(directory, name), `${JSON.stringify(value, null, 2)}\n`);
  const manifest = readJson('package.json');
  const originalLock = readJson('package-lock.json');
  for (const location of ['', ...manifest.workspaces]) {
    const name = path.join(location, 'package.json');
    const item = readJson(name);
    delete item.devDependencies;
    writeJson(name, item);
  }
  const productionLock = structuredClone(originalLock);
  for (const item of Object.values(productionLock.packages)) delete item.devDependencies;
  writeJson('package-lock.json', productionLock);

  const npm = (args) => execFileSync(process.platform === 'win32' ? 'bash' : 'npm',
    process.platform === 'win32' ? ['-c', 'exec npm "$@"', 'npm', ...args] : args,
    { cwd: directory, stdio: 'inherit' });
  const flags = ['--workspace', workspace, '--omit=dev', '--offline', '--ignore-scripts', '--no-audit', '--no-fund'];
  // Recompute reachability without development roots, preserving production
  // optional dependencies (including platform-specific native binaries).
  npm(['install', '--package-lock-only', ...flags]);
  const resolvedLock = readJson('package-lock.json');
  for (const [location, item] of Object.entries(resolvedLock.packages)) {
    if (!location.includes('node_modules/') || item.link) continue;
    const original = originalLock.packages[location];
    for (const field of ['version', 'resolved', 'integrity']) {
      if (!original || item[field] !== original[field]) {
        throw new Error(`Runtime staging changed locked ${field} for ${location}`);
      }
    }
  }
  npm(['ci', ...flags]);
}

if (import.meta.main) {
  const [directory, workspace] = process.argv.slice(2);
  if (!directory || !workspace) {
    throw new Error('usage: install-runtime-dependencies.mjs <staging-directory> <workspace>');
  }
  installRuntimeDependencies(directory, workspace);
}
