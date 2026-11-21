import { defineConfig } from "drizzle-kit";
import { relative } from "node:path";
import { fileURLToPath } from "node:url";

export default defineConfig({
  dialect: "postgresql",
  schema: fileURLToPath(new URL("./schema.ts", import.meta.url)),
  out: relative(process.cwd(), fileURLToPath(new URL("./migrations", import.meta.url))),
  schemaFilter: ["artifacts"],
});
