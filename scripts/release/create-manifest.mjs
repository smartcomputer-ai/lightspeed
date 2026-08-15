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

const manifest = {
  manifestVersion: 1,
  version,
  gitSha,
  rustVersion,
  target: metadata.LIGHTSPEED_RELEASE_TARGET,
  buildImage,
  protocolVersion: metadata.LIGHTSPEED_API_PROTOCOL_VERSION,
  contractRevision: `sha256:${contractHash.digest("hex")}`,
  schemaRevision: Number(metadata.LIGHTSPEED_SCHEMA_REVISION),
  platformSchemaRevision: Number(metadata.LIGHTSPEED_PLATFORM_SCHEMA_REVISION),
  platformUpgradeFrom: metadata.LIGHTSPEED_PLATFORM_UPGRADE_FROM,
  images: {
    server: process.env.LIGHTSPEED_SERVER_IMAGE ?? null,
    configuratorMcp: process.env.LIGHTSPEED_CONFIGURATOR_MCP_IMAGE ?? null,
    platform: process.env.LIGHTSPEED_PLATFORM_IMAGE ?? null,
    channelsWorkflows: process.env.LIGHTSPEED_CHANNELS_WORKFLOWS_IMAGE ?? null,
    channelsActivities: process.env.LIGHTSPEED_CHANNELS_ACTIVITIES_IMAGE ?? null,
    channelsTelegram: process.env.LIGHTSPEED_CHANNELS_TELEGRAM_IMAGE ?? null,
    channelsWhatsapp: process.env.LIGHTSPEED_CHANNELS_WHATSAPP_IMAGE ?? null,
  },
  binaries: {
    server: archive("SERVER", "-server-"),
    providerIncus: archive("PROVIDER_INCUS", "-provider-incus-"),
    envd: archive("ENVD", "-envd-"),
    cli: archive("CLI", "-cli-"),
  },
  typescriptClient: {
    name: "@lightspeed/agent-client",
    version,
    sha256: sha256(path.join("dist/npm", clientFile)),
  },
};
fs.writeFileSync("dist/release-manifest.json", `${JSON.stringify(manifest, null, 2)}\n`);
