#!/usr/bin/env node
import fs from 'node:fs';
import { execFileSync } from 'node:child_process';
import { readMetadata, targets, archiveName, urlKey } from './envd-artifacts.mjs';

const metadata = readMetadata();
const root = process.env.GHCR_ROOT;
const tag = process.env.STAGING_TAG;
if (!/^ghcr\.io\/[a-z0-9_/-]+$/.test(root ?? '') || !/^staging-[0-9]+-[0-9]+$/.test(tag ?? '')) {
  throw new Error('expected GHCR_ROOT and run-specific STAGING_TAG');
}
const files = fs.readdirSync('dist/archives');
const specs = ['runtime', 'provider-incus', 'cli', 'demo', 'docs'].map((name) => {
  const matches = files.filter((file) => file.startsWith(`lightspeed-${name}-${metadata.LIGHTSPEED_PRODUCT_VERSION}-`) ||
    file === `lightspeed-${name}-${metadata.LIGHTSPEED_PRODUCT_VERSION}.tar.gz`);
  if (matches.length !== 1) throw new Error(`expected exactly one ${name} archive`);
  return { file: matches[0], repository: `${name}-bundle`, key: `${['demo', 'docs'].includes(name) ? 'LIGHTSPEED_ARTIFACT' : 'LIGHTSPEED_BINARY'}_URL_${name.toUpperCase().replaceAll('-', '_')}` };
});
for (const target of targets(metadata)) {
  const file = archiveName(metadata.LIGHTSPEED_PRODUCT_VERSION, target);
  if (!files.includes(file)) throw new Error(`missing ${target}`);
  specs.push({ file, repository: target === metadata.LIGHTSPEED_ENVD_TARGET ? 'envd-bundle' : `envd-${target}-bundle`, key: urlKey(target) });
}
for (const { file, repository, key } of specs) {
  const ref = `${root}/${repository}:${tag}`;
  execFileSync('oras', ['push', ref, `dist/archives/${file}:application/gzip`], { stdio: 'inherit' });
  const digest = execFileSync('oras', ['resolve', ref], { encoding: 'utf8' }).trim();
  if (!/^sha256:[0-9a-f]{64}$/.test(digest)) throw new Error('invalid registry digest');
  fs.appendFileSync(process.env.GITHUB_ENV, `${key}=oci://${root}/${repository}@${digest}\n`);
}
