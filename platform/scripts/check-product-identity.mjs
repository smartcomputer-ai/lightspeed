import { readFileSync } from "node:fs";
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
