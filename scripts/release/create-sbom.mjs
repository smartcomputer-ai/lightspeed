#!/usr/bin/env node
import fs from "node:fs";
import { execFileSync } from "node:child_process";

const [version, gitSha] = process.argv.slice(2);
if (!version || !gitSha) throw new Error("usage: create-sbom.mjs <version> <git-sha>");
const cargo = JSON.parse(execFileSync("cargo", ["metadata", "--locked", "--format-version=1"], {
  encoding: "utf8",
  maxBuffer: 64 * 1024 * 1024,
}));
const dependencies = new Map();
for (const item of cargo.packages) {
  const key = `cargo:${item.name}@${item.version}`;
  dependencies.set(key, { name: item.name, version: item.version, manager: "cargo" });
}
for (const lockFile of ["package-lock.json"]) {
  const lock = JSON.parse(fs.readFileSync(lockFile, "utf8"));
  for (const [location, item] of Object.entries(lock.packages ?? {})) {
    if (!location || !item.version) continue;
    const name = item.name ?? location.replace(/^node_modules\//, "");
    dependencies.set(`npm:${name}@${item.version}`, {
      name,
      version: item.version,
      manager: "npm",
    });
  }
}
const spdxId = (value) => `SPDXRef-${value.replace(/[^A-Za-z0-9.-]/g, "-")}`;
const rootId = "SPDXRef-Lightspeed";
const packages = [{
  SPDXID: rootId,
  name: "lightspeed",
  versionInfo: version,
  downloadLocation: "NOASSERTION",
  filesAnalyzed: false,
  licenseConcluded: "NOASSERTION",
  licenseDeclared: "NOASSERTION",
  copyrightText: "NOASSERTION",
}];
const relationships = [];
for (const [key, item] of [...dependencies].sort(([a], [b]) => a.localeCompare(b))) {
  const id = spdxId(key);
  packages.push({
    SPDXID: id,
    name: item.name,
    versionInfo: item.version,
    downloadLocation: "NOASSERTION",
    filesAnalyzed: false,
    licenseConcluded: "NOASSERTION",
    licenseDeclared: "NOASSERTION",
    copyrightText: "NOASSERTION",
    externalRefs: [{
      referenceCategory: "PACKAGE-MANAGER",
      referenceType: "purl",
      referenceLocator: `pkg:${item.manager}/${encodeURIComponent(item.name)}@${item.version}`,
    }],
  });
  relationships.push({
    spdxElementId: rootId,
    relationshipType: "DEPENDS_ON",
    relatedSpdxElement: id,
  });
}
const created = new Date(Number(process.env.SOURCE_DATE_EPOCH ?? 0) * 1000)
  .toISOString()
  .replace(".000Z", "Z");
const document = {
  spdxVersion: "SPDX-2.3",
  dataLicense: "CC0-1.0",
  SPDXID: "SPDXRef-DOCUMENT",
  name: `lightspeed-${version}`,
  documentNamespace: `https://lightspeed.dev/spdx/${gitSha}`,
  creationInfo: { created, creators: ["Tool: lightspeed-release"] },
  documentDescribes: [rootId],
  packages,
  relationships,
};
fs.writeFileSync("dist/sbom.spdx.json", `${JSON.stringify(document, null, 2)}\n`);
