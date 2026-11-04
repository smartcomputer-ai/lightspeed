import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const root = fileURLToPath(new URL("../../", import.meta.url));

for (const override of ["", "/tmp/internal images/compose.yaml"]) {
  test(`infrastructure commands use ${override ? "the selected" : "the default"} Compose file`, () => {
    const result = spawnSync("bash", ["-c", `
      source scripts/dev/lib/common.sh
      docker() { printf '%s\\n' "$@"; }
      compose ps
    `], {
      cwd: root,
      encoding: "utf8",
      env: { ...process.env, LIGHTSPEED_DEV_COMPOSE_FILE: override },
    });
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(result.stdout.trim().split("\n"), [
      "compose", "-f", override || `${root}scripts/dev/docker-compose.yaml`, "ps",
    ]);
  });
}
