// better-auth's CLI generates `timestamp(...)` columns without timezone
// (Postgres `timestamp without time zone`). We store instants, so every
// timestamp column should be timestamptz. Run this after regenerating
// src/schema/auth.ts to re-apply the option.
import { readFileSync, writeFileSync } from "node:fs";

const path = new URL("../src/schema/auth.ts", import.meta.url);
const source = readFileSync(path, "utf8");
const patched = source.replaceAll(
  /timestamp\("([a-z_]+)"\)/g,
  'timestamp("$1", { withTimezone: true })',
);
writeFileSync(path, patched);
console.log(
  patched === source
    ? "auth.ts already timezone-aware"
    : "auth.ts patched to timestamptz",
);
