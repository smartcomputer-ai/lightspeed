#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const [kind, directory, version, gitSha] = process.argv.slice(2);
if (!kind || !directory || !version || !gitSha) {
  throw new Error("usage: stage-package.mjs <client|configurator> <directory> <version> <git-sha>");
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
  manifest.dependencies["@lightspeed/agent-client"] = "file:./agent-client.tgz";
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
  lock.packages[""].dependencies = manifest.dependencies;
  for (const [location, item] of Object.entries(lock.packages)) {
    if (location && item?.name === "@lightspeed/agent-client") {
      delete lock.packages[location];
    }
  }
  const clientTarball = path.join(directory, "agent-client.tgz");
  if (!fs.existsSync(clientTarball)) {
    throw new Error(`${clientTarball}: staged client tarball is missing`);
  }
  const integrity = crypto
    .createHash("sha512")
    .update(fs.readFileSync(clientTarball))
    .digest("base64");
  lock.packages["node_modules/@lightspeed/agent-client"] = {
    version,
    resolved: "file:agent-client.tgz",
    integrity: `sha512-${integrity}`,
    engines: { node: ">=24" },
  };
}

fs.writeFileSync(lockPath, `${JSON.stringify(lock, null, 2)}\n`);
