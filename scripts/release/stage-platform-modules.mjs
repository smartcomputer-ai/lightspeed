import { cpSync, existsSync, mkdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { standardModules } from '../../distributions/standard/catalog.ts';
import { installRuntimeDependencies, runNpm } from './install-runtime-dependencies.mjs';

const [destination, host] = process.argv.slice(2);
const workspaces = [host];
for (const module of standardModules) {
  const directory = join('modules', module.id);
  const copy = file => {
    const target = join(destination, directory, file);
    mkdirSync(dirname(target), { recursive: true });
    cpSync(join(directory, file), target, { recursive: true });
  };
  if (!existsSync(join(directory, 'package.json'))) {
    // Source-only contributions use the host's dependencies and the conventional directories.
    for (const file of ['module.ts', 'contract.ts', ...(module.api ? ['api'] : []), ...(module.migrations ? ['db'] : [])]) {
      if (existsSync(join(directory, file))) copy(file);
    }
    continue;
  }
  const { name } = JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8'));
  const output = runNpm(['pack', '--dry-run', '--json', '--ignore-scripts', '--workspace', name], { encoding: 'utf8' });
  const [pkg] = JSON.parse(output);
  for (const { path } of pkg.files) copy(path);
  workspaces.push(pkg.name);
}
installRuntimeDependencies(destination, ...workspaces);
