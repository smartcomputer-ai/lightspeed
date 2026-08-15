import { existsSync, readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";

const retiredIdentity = new RegExp(
  `${["ls", "bot"].join("")}|${["ls", "bot"].join("\\.")}`,
  "i",
);
const files = execFileSync("git", ["ls-files", "-co", "--exclude-standard", "-z"], {
  encoding: "utf8",
})
  .split("\0")
  .filter(Boolean);
const violations = [];

for (const file of files) {
  // `git ls-files --cached` also reports tracked paths deleted by an
  // uncommitted move. Scan their replacement paths, not nonexistent entries.
  if (!existsSync(file)) continue;
  const body = readFileSync(file);
  if (body.includes(0)) {
    continue;
  }
  for (const [index, line] of body.toString("utf8").split("\n").entries()) {
    if (retiredIdentity.test(line)) {
      violations.push(`${file}:${index + 1}:${line}`);
    }
  }
}

if (violations.length > 0) {
  console.error("Retired product identity found:");
  console.error(violations.join("\n"));
  process.exitCode = 1;
}
