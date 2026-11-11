#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { readMetadata, validateWindowsBuild } from './envd-artifacts.mjs';

const [source, gitSha] = process.argv.slice(2);
if (!source || !/^[0-9a-f]{40}$/.test(gitSha ?? '')) throw new Error('usage: import-windows.mjs <directory> <git-sha>');
const build = validateWindowsBuild(JSON.parse(fs.readFileSync(path.join(source, 'provenance/windows-envd.json'), 'utf8')),
  readMetadata(), gitSha, path.join(source, 'archives'));
fs.mkdirSync('dist/provenance', { recursive: true });
fs.copyFileSync(path.join(source, 'archives', build.file), path.join('dist/archives', build.file));
fs.copyFileSync(path.join(source, 'provenance/windows-envd.json'), 'dist/provenance/windows-envd.json');
