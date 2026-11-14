import { execFileSync } from 'node:child_process';
import { styleText } from 'node:util';

const numberFormat = new Intl.NumberFormat('en-US');
const formatNumber = value => numberFormat.format(value);
const folderDepth = 2;

const args = process.argv.slice(2);
const usage = 'Usage: npm run git:change-stats -- [base-ref] [--files]\nDefault base: origin/main. Run git fetch origin to refresh it.\n--files, -f: also list every changed file with cumulative line counts.';
if (args.includes('--help')) {
  console.log(usage);
  process.exit(0);
}
const showFiles = args.includes('--files') || args.includes('-f');
const refs = args.filter(arg => arg !== '--files' && arg !== '-f');
if (refs.length > 1 || refs.some(arg => arg.startsWith('-'))) {
  console.error(usage);
  process.exit(1);
}

function git(...args) {
  return execFileSync('git', args, {
    encoding: 'utf8', maxBuffer: 64 * 1024 * 1024,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
}

try {
  const base = refs[0] ?? 'origin/main';
  git('rev-parse', '--verify', `${base}^{commit}`);
  const range = `${base}..HEAD`;
  const commits = Number(git('rev-list', '--count', '--first-parent', '--no-merges', range, '--').trim());
  const records = git('log', '--first-parent', '--no-merges', '--format=', '--numstat', '-z', range, '--').split('\0');
  const folders = new Map();
  const files = new Map();
  let binaries = 0;
  for (let i = 0; i < records.length; i++) {
    if (!records[i].trim()) continue;
    const match = records[i].match(/^\n*(\d+|-)\t(\d+|-)\t([\s\S]*)$/);
    if (!match) throw new Error('Unexpected Git numstat output');
    const [, added, removed, name] = match;
    let path = name;
    // With -z, a rename has an empty path followed by the old and new paths.
    if (!path) {
      i++;
      path = records[++i];
    }
    const parts = path.split('/');
    const folder = parts.slice(0, Math.min(folderDepth, parts.length - 1)).join('/') || '(root)';
    const stats = folders.get(folder) ?? { added: 0, removed: 0 };
    const file = files.get(path) ?? { added: 0, removed: 0, binary: false, text: false };
    if (added === '-') {
      binaries++;
      file.binary = true;
    }
    else {
      stats.added += Number(added);
      stats.removed += Number(removed);
      file.added += Number(added);
      file.removed += Number(removed);
      file.text = true;
    }
    folders.set(folder, stats);
    files.set(path, file);
  }

  console.log(`${styleText('cyan', 'Branch:')} ${git('branch', '--show-current').trim() || '(detached HEAD)'}`);
  console.log(`${styleText('cyan', 'Base:')} ${base} (local ref; no fetch performed)`);
  console.log(`${formatNumber(commits)} first-parent, non-merge commits; folders grouped to ${folderDepth} levels.\n`);
  const rows = [...folders].sort(([a], [b]) => a.localeCompare(b));
  const total = rows.reduce((sum, [, stats]) => ({
    added: sum.added + stats.added, removed: sum.removed + stats.removed,
  }), { added: 0, removed: 0 });
  const width = Math.max(6, ...rows.map(([folder]) => folder.length));
  const numberWidth = Math.max(10, formatNumber(total.added).length, formatNumber(total.removed).length);
  const formatCount = value => typeof value === 'number' ? formatNumber(value) : value;
  const print = (folder, added, removed, labelWidth = width) => console.log(
    `${folder.padEnd(labelWidth)}  ${styleText('green', formatCount(added).padStart(numberWidth))}  ${styleText('red', formatCount(removed).padStart(numberWidth))}`,
  );
  console.log(styleText(['bold', 'cyan'], `${'Folder'.padEnd(width)}  ${'+'.padStart(numberWidth)}  ${'-'.padStart(numberWidth)}`));
  for (const [folder, stats] of rows) print(folder, stats.added, stats.removed);
  print(styleText('bold', 'TOTAL'.padEnd(width)), total.added, total.removed);
  if (showFiles) {
    const fileRows = [...files].sort(([a], [b]) => a.localeCompare(b)).map(([path, stats]) => [
      `${path}${stats.binary ? ' (binary)' : ''}`, stats,
    ]);
    const fileWidth = Math.max(4, ...fileRows.map(([path]) => path.length));
    console.log(styleText(['bold', 'cyan'], `\n${'File'.padEnd(fileWidth)}  ${'+'.padStart(numberWidth)}  ${'-'.padStart(numberWidth)}`));
    for (const [path, stats] of fileRows) {
      print(path, stats.text ? stats.added : '-', stats.text ? stats.removed : '-', fileWidth);
    }
  }
  console.log('\nSums per-commit changes, including repeated edits. Uncommitted changes excluded.');
  if (binaries) console.log(`${formatNumber(binaries)} binary file changes have no line counts.`);
} catch (error) {
  console.error(`git:change-stats: ${error.stderr?.toString().trim() || error.message}`);
  process.exitCode = 1;
}
