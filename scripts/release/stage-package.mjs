#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const [kind, directory, version, gitSha, clientLockPath] = process.argv.slice(2);
if (!kind || !directory || !version || !gitSha) {
  throw new Error("usage: stage-package.mjs <client|configurator> <directory> <version> <git-sha> [client-lockfile]");
}

const packagePath = path.join(directory, "package.json");
const manifest = JSON.parse(fs.readFileSync(packagePath, "utf8"));
manifest.version = version;
manifest.private = false;

if (kind === "client") {
  manifest.publishConfig = { access: "public" };
  fs.writeFileSync(
    path.join(directory, "release.json"),
    `${JSON.stringify({ version, gitSha }, null, 2)}\n`,
  );
} else if (kind === "configurator") {
  manifest.private = true;
  manifest.dependencies["@lightspeed-ai/sdk"] = "file:./sdk.tgz";
  delete manifest.scripts?.prepare;
} else {
  throw new Error(`unknown package kind: ${kind}`);
}

fs.writeFileSync(packagePath, `${JSON.stringify(manifest, null, 2)}\n`);
const lockPath = path.join(directory, "package-lock.json");
if (!fs.existsSync(lockPath)) {
  throw new Error(`${lockPath}: release staging requires a committed npm lockfile`);
}
const lock = JSON.parse(fs.readFileSync(lockPath, "utf8"));
lock.version = version;
if (!lock.packages?.[""]) {
  throw new Error(`${lockPath}: missing root package entry`);
}
lock.packages[""].version = version;

if (kind === "configurator") {
  if (!clientLockPath) {
    throw new Error("Configurator staging requires the packed client's lockfile");
  }
  const clientLock = JSON.parse(fs.readFileSync(clientLockPath, "utf8"));
  const clientMetadata = { ...clientLock.packages?.[""] };
  if (clientMetadata.name !== "@lightspeed-ai/sdk") {
    throw new Error(`${clientLockPath}: expected the SDK's standalone lockfile`);
  }
  delete clientMetadata.devDependencies;
  const clientLocation = "node_modules/@lightspeed-ai/sdk";
  lock.packages[""].dependencies = manifest.dependencies;
  for (const [location, item] of Object.entries(lock.packages)) {
    if ((location && item?.name === "@lightspeed-ai/sdk")
      || location.startsWith(`${clientLocation}/`)) {
      delete lock.packages[location];
    }
  }
  const clientTarball = path.join(directory, "sdk.tgz");
  if (!fs.existsSync(clientTarball)) {
    throw new Error(`${clientTarball}: staged client tarball is missing`);
  }
  const integrity = crypto
    .createHash("sha512")
    .update(fs.readFileSync(clientTarball))
    .digest("base64");
  lock.packages[clientLocation] = {
    ...clientMetadata,
    version,
    resolved: "file:sdk.tgz",
    integrity: `sha512-${integrity}`,
  };
  // Keep the SDK's production graph at its tested versions. Nest it under the
  // SDK so Configurator's own locked dependencies can retain different versions.
  for (const [location, item] of Object.entries(clientLock.packages)) {
    if (location.startsWith("node_modules/") && !item.dev) {
      lock.packages[`${clientLocation}/${location}`] = item;
    }
  }
}

fs.writeFileSync(lockPath, `${JSON.stringify(lock, null, 2)}\n`);
