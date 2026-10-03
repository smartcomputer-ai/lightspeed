import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { classifyChanges, classifyPaths } from './classify-changes.mjs';

const all = { rust: true, consumers: true, docs: true };
const none = { rust: false, consumers: false, docs: false };

test('manual, styles, assets, and included references select docs checks', () => {
  for (const path of [
    'docs/documentation/getting-started/quickstart.md', 'docs/site/src/styles/theme.css',
    'docs/site/astro.config.mjs', 'docs/site/scripts/content.mjs', 'docs/site/package.json',
    'docs/site/public/social.png', 'docs/images/example.png', 'docs/documentation/reference/environment-variables.md',
  ]) {
    assert.deepEqual(classifyPaths([path]), { ...none, docs: true }, path);
  }
  assert.deepEqual(classifyPaths(['crates/api/contract/api-reference.md']), all);
  assert.deepEqual(classifyPaths(['crates/temporal-workflow/contract/workflow-contract.md']),
    { ...none, rust: true, docs: true });
});

test('unrelated code and internal prose do not run docs checks', () => {
  assert.deepEqual(classifyPaths(['crates/harness/src/lib.rs']), { ...none, rust: true });
  assert.deepEqual(classifyPaths(['crates/api/src/lib.rs']), { ...none, rust: true, consumers: true });
  assert.deepEqual(classifyPaths(['platform/web/src/main.tsx', 'clients/typescript/src/index.ts']),
    { ...none, consumers: true });
  assert.deepEqual(classifyPaths(['docs/roadmap/documentation.md', 'docs/site/README.md', 'docs/variables.md', 'README.md']), none);
});

test('shared dependencies, CI, and release inputs select their consumers', () => {
  for (const path of ['package.json', 'package-lock.json']) {
    assert.deepEqual(classifyPaths([path]), { ...none, consumers: true, docs: true });
  }
  for (const path of ['.github/workflows/ci.yml', 'release/build-env.Dockerfile',
    'scripts/release/stage-static-sites.sh', 'new-build-input']) {
    assert.deepEqual(classifyPaths([path]), all, path);
  }
});

test('manual dispatch and initial pushes run all checks; invalid commits fail', () => {
  assert.deepEqual(classifyChanges({ eventName: 'workflow_dispatch' }), all);
  assert.deepEqual(classifyChanges({ eventName: 'push', baseSha: '0'.repeat(40) }), all);
  assert.throws(() => classifyChanges({ eventName: 'push', baseSha: 'bad', headSha: 'bad' }),
    /full base and head commit SHAs/);
});

test('git classification covers pushed ranges, PR merge bases, removals, and renames', (t) => {
  const cwd = mkdtempSync(join(tmpdir(), 'lightspeed-ci-paths-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const git = (...args) => execFileSync('git', args, { cwd, encoding: 'utf8', stdio: 'pipe' }).trim();
  const write = (path, body) => {
    mkdirSync(dirname(join(cwd, path)), { recursive: true });
    writeFileSync(join(cwd, path), body);
  };
  const commit = (message) => {
    git('add', '.');
    git('-c', 'user.name=CI Test', '-c', 'user.email=ci@example.test',
      '-c', 'commit.gpgsign=false', 'commit', '-qm', message);
    return git('rev-parse', 'HEAD');
  };
  git('init', '-q');
  write('docs/documentation/guide with spaces.md', '# Guide\n');
  const baseSha = commit('Initial manual');
  write('crates/harness/src/lib.rs', '// main branch change\n');
  const mainSha = commit('Advance main');
  git('checkout', '-q', '--detach', baseSha);
  mkdirSync(join(cwd, 'docs/internal'), { recursive: true });
  git('mv', 'docs/documentation/guide with spaces.md', 'docs/internal/guide.md');
  const movedSha = commit('Remove guide from the published manual');
  write('platform/web/src/main.tsx', '// unrelated platform change\n');
  const headSha = commit('Update platform');

  assert.deepEqual(classifyChanges({ cwd, eventName: 'push', baseSha, headSha }),
    { ...none, consumers: true, docs: true });
  assert.deepEqual(classifyChanges({ cwd, eventName: 'pull_request', baseSha: mainSha, headSha: movedSha }),
    { ...none, docs: true });
  assert.throws(() => classifyChanges({ cwd, eventName: 'push', baseSha, headSha: 'f'.repeat(40) }));
});
