import { execFileSync } from 'node:child_process';
import { appendFileSync } from 'node:fs';

export function classifyPaths(paths) {
  const checks = { rust: false, consumers: false, docs: false, python: false };
  for (const path of paths) {
    if (/^(crates\/api\/contract\/(api\.schema\.json|methods\.json)$|clients\/(python|fixtures)\/|clients\/typescript\/scripts\/helpers\.mjs$|examples\/streamlit-lightspeed\/)/.test(path)) {
      checks.python = true;
    }
    if (/^(docs\/(documentation\/|images\/|site\/)|crates\/api\/contract\/api-reference\.md$|crates\/temporal-workflow\/contract\/workflow-contract\.md$|package(?:-lock)?\.json$)/.test(path)
      && path !== 'docs/site/README.md') {
      checks.docs = true;
    }

    if (path.startsWith('crates/api/')) {
      checks.rust = checks.consumers = true;
    } else if (/^(crates\/|Cargo\.(toml|lock)$|rust-toolchain\.toml$)/.test(path)) {
      checks.rust = true;
    } else if (/^(clients\/python\/|examples\/streamlit-lightspeed\/)/.test(path)) {
      // The Python check covers the SDK and example.
    } else if (/^(clients\/|platform\/|package(?:-lock)?\.json$|tsconfig\.json$)/.test(path)) {
      checks.consumers = true;
    } else if (/^(\.github\/workflows\/|release\/|scripts\/)/.test(path)) {
      checks.rust = checks.consumers = checks.docs = checks.python = true;
    } else if (/^(docs\/|README\.md$|AGENTS\.md$|CLAUDE\.md$|LICENSE)/.test(path) || path.endsWith('.md')) {
      // Internal prose does not change the product or published manual.
    } else {
      // Check new source and build inputs conservatively until classified.
      checks.rust = checks.consumers = checks.docs = checks.python = true;
    }
  }
  return checks;
}

export function classifyChanges({ eventName, baseSha, headSha, cwd = process.cwd() }) {
  if (eventName === 'workflow_dispatch' || /^0{40}$/.test(baseSha ?? '')) {
    return { rust: true, consumers: true, docs: true, python: true };
  }
  if (!/^[0-9a-f]{40}$/.test(baseSha ?? '') || !/^[0-9a-f]{40}$/.test(headSha ?? '')) {
    throw new Error('CI classification requires full base and head commit SHAs');
  }
  // PRs compare with their merge base; pushes compare the full pushed range.
  // Disable rename detection so removals from a component are classified too.
  const range = eventName === 'pull_request' ? [`${baseSha}...${headSha}`] : [baseSha, headSha];
  const paths = execFileSync('git', ['diff', '--name-only', '--no-renames', '-z', ...range], {
    cwd, encoding: 'utf8', stdio: 'pipe', maxBuffer: 16 * 1024 * 1024,
  }).split('\0').filter(Boolean);
  return classifyPaths(paths);
}

if (import.meta.main) {
  const checks = classifyChanges({
    eventName: process.env.EVENT_NAME,
    baseSha: process.env.BASE_SHA,
    headSha: process.env.HEAD_SHA,
  });
  const output = Object.entries(checks).map(([name, needed]) => `${name}=${needed}\n`).join('');
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, output);
  process.stdout.write(output);
}
