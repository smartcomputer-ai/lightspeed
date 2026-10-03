#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";

const [version, gitSha] = process.argv.slice(2);
if (!version || !/^[0-9a-f]{40}$/.test(gitSha ?? "")) {
  throw new Error("usage: create-manifest.mjs <version> <full-git-sha>");
}

const metadata = Object.fromEntries(
  fs.readFileSync("release/metadata.env", "utf8")
    .split("\n")
    .filter((line) => line && !line.startsWith("#"))
    .map((line) => line.split(/=(.*)/s).slice(0, 2)),
);
const sha256 = (file) => crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
const contractHash = crypto.createHash("sha256");
for (const file of ["api.schema.json", "methods.json", "openrpc.json", "api-reference.md"]) {
  contractHash.update(fs.readFileSync(path.join("dist/contracts", file)));
}

const archive = (key, needle) => {
  const file = fs.readdirSync("dist/archives").find((entry) => entry.includes(needle));
  if (!file) throw new Error(`missing archive matching ${needle}`);
  return {
    file,
    url: process.env[`LIGHTSPEED_BINARY_URL_${key}`] ?? null,
    sha256: sha256(path.join("dist/archives", file)),
  };
};
const artifact = (key, needle) => {
  const file = fs.readdirSync("dist/archives").find((entry) => entry.includes(needle));
  if (!file) throw new Error(`missing artifact matching ${needle}`);
  return {
    file,
    url: process.env[`LIGHTSPEED_ARTIFACT_URL_${key}`] ?? null,
    sha256: sha256(path.join("dist/archives", file)),
  };
};
const clientFile = fs.readdirSync("dist/npm").find((entry) => entry.endsWith(".tgz"));
if (!clientFile) throw new Error("missing TypeScript client tarball");
const existingManifest = fs.existsSync("dist/release-manifest.json")
  ? JSON.parse(fs.readFileSync("dist/release-manifest.json", "utf8"))
  : undefined;
const buildImage = process.env.LIGHTSPEED_RELEASE_BUILD_IMAGE ?? existingManifest?.buildImage;
if (!/@sha256:[0-9a-f]{64}$/.test(buildImage ?? "")) {
  throw new Error("LIGHTSPEED_RELEASE_BUILD_IMAGE must identify the actual digest-pinned build image");
}
const rustVersion = existingManifest?.rustVersion
  ?? execFileSync("rustc", ["--version"], { encoding: "utf8" }).trim();
if (typeof rustVersion !== "string" || rustVersion.length === 0) {
  throw new Error("release manifest must identify the Rust toolchain used by the build environment");
}

// The environment daemon's discovery document. A deployment serves it at a
// well-known path, next to the archives it names, so an orchestrator can pick
// the daemon that matches a gateway without credentials or a checkout. The
// protocol version is the number that decides admission; the rest is
// provenance. `url` is a plain HTTPS download when one exists, which today is
// only a tagged release's GitHub asset; a snapshot bundle carries null and the
// serving deployment fills it in.
const envdTarget = metadata.LIGHTSPEED_ENVD_TARGET;
const environmentProtocolVersion = Number(metadata.LIGHTSPEED_ENVIRONMENT_PROTOCOL_VERSION);
if (!envdTarget || !Number.isInteger(environmentProtocolVersion) || environmentProtocolVersion < 1) {
  throw new Error("release metadata must name the envd target and the environment protocol version");
}
const channel = process.env.LIGHTSPEED_RELEASE_CHANNEL ?? "main";
if (!["release", "main"].includes(channel)) {
  throw new Error("LIGHTSPEED_RELEASE_CHANNEL must be release or main");
}
const publicUrlBase = process.env.LIGHTSPEED_ENVD_PUBLIC_URL_BASE || null;
if (publicUrlBase !== null && !/^https:\/\/\S+[^/]$/.test(publicUrlBase)) {
  throw new Error("LIGHTSPEED_ENVD_PUBLIC_URL_BASE must be an https URL without a trailing slash");
}
const builtAtMs = process.env.SOURCE_DATE_EPOCH
  ? Number(process.env.SOURCE_DATE_EPOCH) * 1000
  : Date.now();
const envdPrefix = `lightspeed-envd-${version}-`;
const envdArtifacts = Object.fromEntries(
  fs.readdirSync("dist/archives")
    .filter((entry) => entry.startsWith(envdPrefix) && entry.endsWith(".tar.gz"))
    .sort()
    .map((file) => [
      file.slice(envdPrefix.length, -".tar.gz".length),
      {
        file,
        sha256: sha256(path.join("dist/archives", file)),
        url: publicUrlBase ? `${publicUrlBase}/${file}` : null,
      },
    ]),
);
if (!envdArtifacts[envdTarget]) throw new Error(`missing envd archive for ${envdTarget}`);
const discovery = {
  version,
  gitSha,
  channel,
  protocolVersion: environmentProtocolVersion,
  builtAtMs,
  artifacts: envdArtifacts,
};
fs.writeFileSync("dist/envd.json", `${JSON.stringify(discovery, null, 2)}\n`);

const manifest = {
  manifestVersion: 1,
  version,
  gitSha,
  rustVersion,
  target: metadata.LIGHTSPEED_RELEASE_TARGET,
  envdTarget,
  buildImage,
  protocolVersion: metadata.LIGHTSPEED_API_PROTOCOL_VERSION,
  environmentProtocolVersion,
  contractRevision: `sha256:${contractHash.digest("hex")}`,
  schemaRevision: Number(metadata.LIGHTSPEED_SCHEMA_REVISION),
  platformSchemaRevision: Number(metadata.LIGHTSPEED_PLATFORM_SCHEMA_REVISION),
  platformUpgradeFrom: metadata.LIGHTSPEED_PLATFORM_UPGRADE_FROM,
  images: {
    runtime: process.env.LIGHTSPEED_RUNTIME_IMAGE ?? null,
    configuratorMcp: process.env.LIGHTSPEED_CONFIGURATOR_MCP_IMAGE ?? null,
    platform: process.env.LIGHTSPEED_PLATFORM_IMAGE ?? null,
    platformWorkers: process.env.LIGHTSPEED_PLATFORM_WORKERS_IMAGE ?? null,
  },
  binaries: {
    runtime: archive("RUNTIME", "-runtime-"),
    providerIncus: archive("PROVIDER_INCUS", "-provider-incus-"),
    envd: archive("ENVD", "-envd-"),
    cli: archive("CLI", "-cli-"),
  },
  artifacts: {
    demo: {
      ...artifact("DEMO", "-demo-"),
      basePath: "/demo/",
    },
    docs: {
      ...artifact("DOCS", "-docs-"),
      basePath: "/docs/",
    },
  },
  typescriptClient: {
    name: "@lightspeed-ai/sdk",
    version,
    sha256: sha256(path.join("dist/npm", clientFile)),
  },
};
fs.writeFileSync("dist/release-manifest.json", `${JSON.stringify(manifest, null, 2)}\n`);
