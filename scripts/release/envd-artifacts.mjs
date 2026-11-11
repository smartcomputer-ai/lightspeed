import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';

export const hash = (file) => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
export const readMetadata = () => Object.fromEntries(fs.readFileSync('release/metadata.env', 'utf8')
  .split(/\r?\n/).filter((line) => line && !line.startsWith('#'))
  .map((line) => line.split(/=(.*)/s).slice(0, 2)));
export const targets = (metadata) => {
  const result = metadata.LIGHTSPEED_ENVD_TARGETS.split(',');
  if (new Set(result).size !== result.length || result.some((t) => !/^[a-z0-9_]+(-[a-z0-9_]+)+$/.test(t))) {
    throw new Error('invalid required envd targets');
  }
  return result;
};
export const archiveName = (version, target) => `lightspeed-envd-${version}-${target}.tar.gz`;
export const urlKey = (target) => `LIGHTSPEED_BINARY_URL_ENVD_${target.toUpperCase().replaceAll('-', '_')}`;

export function validateWindowsBuild(build, metadata, gitSha, archiveDirectory) {
  const target = 'x86_64-pc-windows-msvc';
  const expected = {
    version: metadata.LIGHTSPEED_PRODUCT_VERSION, gitSha, target,
    protocolVersion: Number(metadata.LIGHTSPEED_ENVD_PROTOCOL_VERSION),
    file: archiveName(metadata.LIGHTSPEED_PRODUCT_VERSION, target),
  };
  for (const [key, value] of Object.entries(expected)) {
    if (build[key] !== value) throw new Error(`Windows build ${key} mismatch`);
  }
  if (build.sha256 !== hash(path.join(archiveDirectory, expected.file))) throw new Error('Windows archive checksum mismatch');
  if (!build.rustVersion?.startsWith(`rustc ${metadata.LIGHTSPEED_RELEASE_RUST_VERSION} `)) throw new Error('Windows Rust version mismatch');
  if (build.rustFlags !== '-C target-feature=+crt-static') throw new Error('Windows release must use the static C runtime');
  for (const key of ['osVersion', 'msvcVersion', 'windowsSdkVersion', 'linkerVersion', 'cargoVersion']) {
    if (typeof build[key] !== 'string' || !build[key].trim()) throw new Error(`Windows provenance missing ${key}`);
  }
  if (!/^[0-9a-f]{64}$/.test(build.cargoLockSha256 ?? '') ||
      build.cargoLockSha256 !== hash('Cargo.lock')) throw new Error('Windows Cargo.lock checksum mismatch');
  return build;
}
