import { readFile } from "node:fs/promises";
import { expect, it } from "vitest";
import { CLIENT_HELPERS } from "../src/index.js";

// Plain JavaScript so the package's own TypeScript stays free of Node's types,
// which is part of what keeps the client usable in a browser.
it("documents every helper, generated or hand-written", async () => {
  const readme = await readFile(new URL("../README.md", import.meta.url), "utf8");
  const handWritten = ["startRun", "steerRun", "awaitRun", "readEvents", "readBlobText", "readBlobBytes", "putBlob"];
  for (const helper of [...Object.keys(CLIENT_HELPERS), ...handWritten]) {
    expect(readme, helper).toContain(`\`${helper}\``);
  }
});
