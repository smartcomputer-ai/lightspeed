import { readFile, writeFile, copyFile, mkdir } from "node:fs/promises";
import { compileFromFile } from "json-schema-to-typescript";
const root = new URL("../", import.meta.url);
const contract = new URL("../../../platform/shared/contract/", import.meta.url);
const bannerComment = "/* Generated from platform/shared/contract by scripts/generate-platform.mjs. Do not edit. */";
export async function generatePlatform() {
  const { operations } = JSON.parse(await readFile(new URL("http.json", contract), "utf8"));
  await mkdir(new URL("src/generated/", root), { recursive: true });
  await mkdir(new URL("schema/", root), { recursive: true });
  await writeFile(new URL("src/generated/platform-types.ts", root), await compileFromFile(new URL("platform.schema.json", contract).pathname, { bannerComment, unreachableDefinitions: true, additionalProperties: false, enableConstEnums: false }));
  await writeFile(new URL("src/generated/platform.ts", root), `${bannerComment}\nimport type * as T from "./platform-types.js";\nexport interface PlatformOperationMap {\n${operations.map(o => `  ${JSON.stringify(o.operationId)}: { params: T.${o.params}; result: T.${o.result} };`).join("\n")}\n}\nexport type PlatformOperationId = keyof PlatformOperationMap;\nexport type PlatformOperationParams<K extends PlatformOperationId> = PlatformOperationMap[K]["params"];\nexport type PlatformOperationResult<K extends PlatformOperationId> = PlatformOperationMap[K]["result"];\nexport const PLATFORM_OPERATIONS = ${JSON.stringify(Object.fromEntries(operations.map(({ operationId, method, path }) => [operationId, { method, path }])), null, 2)} as const;\n`);
  await copyFile(new URL("platform.schema.json", contract), new URL("schema/platform.schema.json", root));
  await copyFile(new URL("http.json", contract), new URL("schema/platform-http.json", root));
}
