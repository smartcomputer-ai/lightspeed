import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import {
  EXCEPTIONS_PATH, checkPackageAges, fetchPublishTimes, formatReport, lockedPackages, parseExceptions,
} from './check-package-age.mjs';

const DAY_MS = 86_400_000;
const NOW = Date.parse('2026-11-13T12:00:00Z');
const daysAgo = (days) => new Date(NOW - days * DAY_MS).toISOString();
const registry = (name, version) => ({
  version, resolved: `https://registry.npmjs.org/${name}/-/${name.split('/').pop()}-${version}.tgz`,
});
const lockOf = (packages) => ({ lockfileVersion: 3, packages: { '': { name: 'root' }, ...packages } });
const check = (packages, times, options = {}) => checkPackageAges({
  lockfiles: { 'package-lock.json': lockOf(packages) },
  publishTimes: async (name) => times[name] ?? {},
  now: NOW,
  ...options,
});

test('collects installed registry packages and leaves first-party entries out', () => {
  const lock = lockOf({
    'platform/web': { version: '0.8.0' },
    'node_modules/@lightspeed-ai/platform-web': { resolved: 'platform/web', link: true },
    'node_modules/left': registry('left', '1.0.0'),
    'node_modules/left/node_modules/@scope/nested': registry('@scope/nested', '2.1.0'),
    'node_modules/alias': { name: 'real', ...registry('real', '3.0.0') },
    'node_modules/left/node_modules/bundled': { version: '0.1.0', inBundle: true },
    'node_modules/from-git': { version: '1.2.3', resolved: 'git+ssh://git@example.com/org/from-git.git#abc' },
  });
  assert.deepEqual(lockedPackages(lock), [
    { name: 'left', version: '1.0.0', registry: true },
    { name: '@scope/nested', version: '2.1.0', registry: true },
    { name: 'real', version: '3.0.0', registry: true },
    { name: 'from-git', version: '1.2.3', registry: false },
  ]);
  assert.throws(() => lockedPackages({ lockfileVersion: 1, dependencies: {} }), /lockfileVersion 2 or later/);
});

test('a version passes from the moment it reaches the minimum age', async () => {
  const result = await check({
    'node_modules/old': registry('old', '1.0.0'),
    'node_modules/exact': registry('exact', '1.0.0'),
    'node_modules/young': registry('young', '1.0.0'),
  }, {
    old: { '1.0.0': daysAgo(400) },
    exact: { '1.0.0': daysAgo(7) },
    young: { '1.0.0': daysAgo(6.5), '0.9.0': daysAgo(90) },
  });
  assert.equal(result.checked, 3);
  assert.deepEqual(result.violations, [{
    package: 'young@1.0.0',
    problem: 'was published 2026-11-07 00:00 UTC and is allowed from 2026-11-14 00:00 UTC',
    lockfiles: ['package-lock.json'],
  }]);
});

test('a version whose age cannot be established fails', async () => {
  const result = await check({
    'node_modules/withdrawn': registry('withdrawn', '1.0.0'),
    'node_modules/from-git': { version: '1.2.3', resolved: 'git+ssh://git@example.com/org/from-git.git#abc' },
  }, { withdrawn: { '0.9.0': daysAgo(90) } });
  assert.deepEqual(result.violations.map(({ package: id, problem }) => [id, problem]), [
    ['from-git@1.2.3', 'is not installed from the npm registry, so its age cannot be checked'],
    ['withdrawn@1.0.0', 'has no publish time in the npm registry'],
  ]);
});

test('every lockfile is checked and a finding names the lockfiles that hold it', async () => {
  const result = await checkPackageAges({
    lockfiles: {
      'package-lock.json': lockOf({ 'node_modules/young': registry('young', '1.0.0') }),
      'clients/typescript/package-lock.json': lockOf({
        'node_modules/young': registry('young', '1.0.0'),
        'node_modules/old': registry('old', '1.0.0'),
      }),
    },
    publishTimes: async (name) => ({ '1.0.0': daysAgo(name === 'young' ? 1 : 30) }),
    now: NOW,
  });
  assert.equal(result.checked, 2);
  assert.deepEqual(result.violations.map((finding) => finding.lockfiles),
    [['clients/typescript/package-lock.json', 'package-lock.json']]);
});

test('an exception covers one exact version and is reported once it is no longer needed', async () => {
  const exceptions = parseExceptions([
    { package: 'young@1.0.0', reason: 'fixes an exploited vulnerability' },
    { package: '@scope/aged@2.0.0', reason: 'was urgent last month' },
  ]);
  const result = await check({
    'node_modules/young': registry('young', '1.0.0'),
    'node_modules/other/node_modules/young': registry('young', '1.0.1'),
    'node_modules/@scope/aged': registry('@scope/aged', '2.0.0'),
  }, {
    young: { '1.0.0': daysAgo(1), '1.0.1': daysAgo(0.5) },
    '@scope/aged': { '2.0.0': daysAgo(30) },
  }, { exceptions });
  assert.deepEqual(result.excepted.map(({ package: id, reason }) => [id, reason]),
    [['young@1.0.0', 'fixes an exploited vulnerability']]);
  assert.deepEqual(result.violations.map((finding) => finding.package), ['young@1.0.1']);
  assert.deepEqual(result.unused, ['@scope/aged@2.0.0']);

  const report = formatReport(result);
  assert.equal(report.ok, false);
  assert.match(report.lines.join('\n'), /Excepted: young@1\.0\.0 .*Reason: fixes an exploited vulnerability/);
  assert.match(report.lines.join('\n'), /Exception no longer needed: @scope\/aged@2\.0\.0/);
  assert.match(report.lines.join('\n'), /young@1\.0\.1 was published .* \(package-lock\.json\)/);
});

test('a passing report states how many versions were checked', async () => {
  const result = await check({ 'node_modules/old': registry('old', '1.0.0') }, { old: { '1.0.0': daysAgo(8) } });
  assert.deepEqual(formatReport(result),
    { ok: true, lines: ['Package age: 1 locked npm versions checked; none is younger than 7 days.'] });
});

test('exceptions need an exact version and a reason', () => {
  assert.throws(() => parseExceptions({}), /must be a JSON array/);
  for (const entry of [{ package: 'left', reason: 'r' }, { package: 'left@^1.0.0 ', reason: 'r' }, { reason: 'r' }, 'left@1.0.0']) {
    assert.throws(() => parseExceptions([entry]), /exact <name>@<version>/, JSON.stringify(entry));
  }
  for (const entry of [{ package: 'left@1.0.0' }, { package: '@scope/left@1.0.0', reason: '  ' }]) {
    assert.throws(() => parseExceptions([entry]), /needs a "reason"/, JSON.stringify(entry));
  }
});

test('the committed exceptions file is valid', () => {
  const committed = JSON.parse(readFileSync(new URL(`../../${EXCEPTIONS_PATH}`, import.meta.url), 'utf8'));
  assert.doesNotThrow(() => parseExceptions(committed));
});

test('registry reads retry transient failures and give up on permanent ones', async () => {
  const responses = [
    () => { throw new Error('socket hang up'); },
    () => new Response('busy', { status: 503 }),
    () => Response.json({ time: { '1.0.0': daysAgo(9) } }),
  ];
  const urls = [];
  const fetch = async (url) => { urls.push(url); return responses[urls.length - 1](); };
  assert.deepEqual(await fetchPublishTimes('@scope/name', { fetch, delayMs: 0 }), { '1.0.0': daysAgo(9) });
  assert.deepEqual(urls, Array(3).fill('https://registry.npmjs.org/@scope%2Fname'));

  assert.deepEqual(await fetchPublishTimes('gone', { fetch: async () => new Response('', { status: 404 }), delayMs: 0 }), {});

  let calls = 0;
  const forbidden = async () => { calls += 1; return new Response('', { status: 403 }); };
  await assert.rejects(fetchPublishTimes('private', { fetch: forbidden, delayMs: 0 }),
    /could not read private from the npm registry: HTTP 403/);
  assert.equal(calls, 1);

  calls = 0;
  const down = async () => { calls += 1; return new Response('', { status: 502 }); };
  await assert.rejects(fetchPublishTimes('left', { fetch: down, attempts: 3, delayMs: 0 }), /HTTP 502/);
  assert.equal(calls, 3);
});
