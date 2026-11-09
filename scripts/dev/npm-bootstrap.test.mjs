import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

test("the launcher reuses setup's npm install until the lockfile changes", (t) => {
  const directory = mkdtempSync(path.join(tmpdir(), "lightspeed-npm-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  mkdirSync(path.join(directory, "bin"));
  const lockfile = path.join(directory, "package-lock.json");
  writeFileSync(lockfile, "initial lockfile");
  writeFileSync(path.join(directory, "bin/npm"), `#!/bin/sh
set -eu
printf '%s\\n' "$1" >> npm-invocations
mkdir -p node_modules/.bin
for tool in tsx vite astro; do
  touch "node_modules/.bin/$tool"
  chmod +x "node_modules/.bin/$tool"
done
`, { mode: 0o755 });

  const helper = fileURLToPath(new URL("./lib/npm.sh", import.meta.url));
  function bootstrap(mode) {
    const result = spawnSync("bash", ["-c", 'source "$1"; ensure_dev_npm_dependencies "$2"', "bash", helper, mode], {
      cwd: directory,
      env: { ...process.env, PATH: [path.join(directory, "bin"), path.dirname(process.execPath), process.env.PATH].join(path.delimiter) },
      encoding: "utf8",
    });
    assert.equal(result.status, 0, result.stderr);
  }

  bootstrap("ci");
  bootstrap("install");
  assert.equal(readFileSync(path.join(directory, "npm-invocations"), "utf8"), "ci\n");

  writeFileSync(lockfile, "updated lockfile");
  bootstrap("install");
  assert.equal(readFileSync(path.join(directory, "npm-invocations"), "utf8"), "ci\ninstall\n");
});
