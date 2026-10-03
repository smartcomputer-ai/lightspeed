#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const args = process.argv.slice(2);
const file = args.find((argument) => !argument.startsWith("--")) ?? "dist/release-manifest.json";
const published = args.includes("--published");
const value = JSON.parse(fs.readFileSync(file, "utf8"));
const fail = (message) => { throw new Error(`${file}: ${message}`); };
const sha256 = (filename) => crypto
  .createHash("sha256")
  .update(fs.readFileSync(filename))
  .digest("hex");
const metadata = Object.fromEntries(
  fs.readFileSync("release/metadata.env", "utf8")
    .split("\n")
    .filter((line) => line && !line.startsWith("#"))
    .map((line) => line.split(/=(.*)/s).slice(0, 2)),
);
if (value.manifestVersion !== 1) fail("unsupported manifestVersion");
if (!/^[0-9a-f]{40}$/.test(value.gitSha)) fail("gitSha must be a full SHA");
if (value.version !== metadata.LIGHTSPEED_PRODUCT_VERSION) fail("unexpected product version");
if (value.target !== metadata.LIGHTSPEED_RELEASE_TARGET) fail("unexpected target");
if (value.envdTarget !== metadata.LIGHTSPEED_ENVD_TARGET) fail("unexpected envd target");
if (value.environmentProtocolVersion !== Number(metadata.LIGHTSPEED_ENVIRONMENT_PROTOCOL_VERSION)) {
  fail("unexpected environment protocol version");
}
if (!/@sha256:[0-9a-f]{64}$/.test(value.buildImage)) fail("buildImage is not digest-pinned");
if (published && !value.buildImage.startsWith("ghcr.io/")) fail("published build image is not in GHCR");
if (value.protocolVersion !== metadata.LIGHTSPEED_API_PROTOCOL_VERSION) fail("unexpected protocol version");
if (value.schemaRevision !== Number(metadata.LIGHTSPEED_SCHEMA_REVISION)) fail("unexpected schema revision");
if (value.platformSchemaRevision !== Number(metadata.LIGHTSPEED_PLATFORM_SCHEMA_REVISION)) {
  fail("unexpected platform schema revision");
}
if (value.platformUpgradeFrom !== metadata.LIGHTSPEED_PLATFORM_UPGRADE_FROM) {
  fail("unexpected platform upgrade baseline");
}
if (!value.rustVersion.includes(metadata.LIGHTSPEED_RELEASE_RUST_VERSION)) fail("unexpected Rust version");
const contractHash = crypto.createHash("sha256");
for (const contract of ["api.schema.json", "methods.json", "openrpc.json", "api-reference.md"]) {
  contractHash.update(fs.readFileSync(path.join("dist/contracts", contract)));
}
if (value.contractRevision !== `sha256:${contractHash.digest("hex")}`) fail("contract checksum mismatch");
const expectedImages = [
  "runtime",
  "configuratorMcp",
  "platform",
  "platformWorkers",
];
if (JSON.stringify(Object.keys(value.images ?? {}).sort()) !== JSON.stringify(expectedImages.sort())) {
  fail("images must contain the complete runtime artifact set");
}
for (const [name, image] of Object.entries(value.images)) {
  if (image !== null && !/@sha256:[0-9a-f]{64}$/.test(image)) fail(`image ${name} is not digest-pinned`);
  if (published && image === null) fail(`published image ${name} is missing`);
}
for (const [name, artifact] of Object.entries(value.binaries)) {
  if (!/^[0-9a-f]{64}$/.test(artifact.sha256)) fail(`binary ${name} has an invalid checksum`);
  if (path.basename(artifact.file) !== artifact.file) fail(`binary ${name} has an unsafe filename`);
  const archive = `dist/archives/${artifact.file}`;
  if (!fs.existsSync(archive)) fail(`binary ${name} archive is missing`);
  if (sha256(archive) !== artifact.sha256) fail(`binary ${name} checksum mismatch`);
  if (published && !/^oci:\/\/.*@sha256:[0-9a-f]{64}$/.test(artifact.url ?? "")) {
    fail(`published binary ${name} has no immutable OCI URL`);
  }
}
const expectedArtifacts = ["demo", "docs"];
if (JSON.stringify(Object.keys(value.artifacts ?? {}).sort()) !== JSON.stringify(expectedArtifacts)) {
  fail("artifacts must contain the demo and documentation static sites");
}
for (const [name, artifact] of Object.entries(value.artifacts)) {
  if (!/^[0-9a-f]{64}$/.test(artifact.sha256)) fail(`artifact ${name} has an invalid checksum`);
  if (path.basename(artifact.file) !== artifact.file) fail(`artifact ${name} has an unsafe filename`);
  const archive = `dist/archives/${artifact.file}`;
  if (!fs.existsSync(archive)) fail(`artifact ${name} archive is missing`);
  if (sha256(archive) !== artifact.sha256) fail(`artifact ${name} checksum mismatch`);
  if (published && !/^oci:\/\/.*@sha256:[0-9a-f]{64}$/.test(artifact.url ?? "")) {
    fail(`published artifact ${name} has no immutable OCI URL`);
  }
}
if (value.artifacts.demo.basePath !== "/demo/") fail("demo artifact must be served under /demo/");
if (value.artifacts.docs.basePath !== "/docs/") fail("docs artifact must be served under /docs/");
if (!value.binaries.envd.file.endsWith(`-${value.envdTarget}.tar.gz`)) {
  fail("envd archive is not built for the static target");
}

// The discovery document must describe exactly the daemon archives in this
// bundle and agree with the manifest on identity and protocol.
const discoveryFile = path.join(path.dirname(file), "envd.json");
const dfail = (message) => { throw new Error(`${discoveryFile}: ${message}`); };
if (!fs.existsSync(discoveryFile)) dfail("missing");
const discovery = JSON.parse(fs.readFileSync(discoveryFile, "utf8"));
if (discovery.version !== value.version) dfail("version differs from the manifest");
if (discovery.gitSha !== value.gitSha) dfail("gitSha differs from the manifest");
if (!["release", "main"].includes(discovery.channel)) dfail("channel must be release or main");
if (discovery.protocolVersion !== value.environmentProtocolVersion) {
  dfail("protocolVersion differs from the manifest");
}
if (!Number.isInteger(discovery.builtAtMs) || discovery.builtAtMs <= 0) dfail("invalid builtAtMs");
const discoveryTargets = Object.keys(discovery.artifacts ?? {});
if (!discoveryTargets.includes(value.envdTarget)) dfail("no artifact for the envd target");
for (const [target, artifact] of Object.entries(discovery.artifacts)) {
  if (!/^[a-z0-9_]+(-[a-z0-9_]+)+$/.test(target)) dfail(`invalid target ${target}`);
  if (path.basename(artifact.file) !== artifact.file) dfail(`artifact ${target} has an unsafe filename`);
  if (!artifact.file.endsWith(`-${target}.tar.gz`)) dfail(`artifact ${target} file does not name its target`);
  const archive = `dist/archives/${artifact.file}`;
  if (!fs.existsSync(archive)) dfail(`artifact ${target} archive is missing`);
  if (sha256(archive) !== artifact.sha256) dfail(`artifact ${target} checksum mismatch`);
  const plainDownload = typeof artifact.url === "string"
    && artifact.url.startsWith("https://")
    && artifact.url.endsWith(`/${artifact.file}`);
  if (artifact.url !== null && !plainDownload) dfail(`artifact ${target} url is not a plain https download`);
  if (published && discovery.channel === "release" && artifact.url === null) {
    dfail(`published release artifact ${target} has no public download`);
  }
}
if (discovery.artifacts[value.envdTarget].sha256 !== value.binaries.envd.sha256) {
  dfail("envd checksum differs from the manifest");
}
if (value.typescriptClient.name !== "@lightspeed-ai/sdk") fail("unexpected client package");
if (!/^[0-9a-f]{64}$/.test(value.typescriptClient.sha256)) fail("invalid client checksum");
if (value.typescriptClient.version !== value.version) fail("client version mismatch");
const clientFiles = fs.readdirSync("dist/npm").filter((entry) => entry.endsWith(".tgz"));
if (clientFiles.length !== 1) fail("expected exactly one client tarball");
if (sha256(path.join("dist/npm", clientFiles[0])) !== value.typescriptClient.sha256) {
  fail("client checksum mismatch");
}
