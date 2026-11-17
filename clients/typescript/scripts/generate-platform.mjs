import { readFile, writeFile, copyFile, mkdir } from "node:fs/promises";
import { compileFromFile } from "json-schema-to-typescript";
const root = new URL("../", import.meta.url);
const contract = new URL("../../../platform/shared/contract/", import.meta.url);
const bannerComment = "/* Generated from platform/shared/contract by scripts/generate-platform.mjs. Do not edit. */";
export async function generatePlatform() {
  const { operations } = JSON.parse(await readFile(new URL("http.json", contract), "utf8"));
  const schema = JSON.parse(await readFile(new URL("platform.schema.json", contract), "utf8"));
  await mkdir(new URL("src/generated/", root), { recursive: true });
  await mkdir(new URL("schema/", root), { recursive: true });
  await writeFile(new URL("src/generated/platform-types.ts", root), await compileFromFile(new URL("platform.schema.json", contract).pathname, { bannerComment, unreachableDefinitions: true, additionalProperties: false, enableConstEnums: false }));
  await writeFile(new URL("src/generated/platform.ts", root), `${bannerComment}\nimport type * as T from "./platform-types.js";\nexport interface PlatformOperationMap {\n${operations.map(o => `  ${JSON.stringify(o.operationId)}: { params: T.${o.params}; result: T.${o.result} };`).join("\n")}\n}\nexport type PlatformOperationId = keyof PlatformOperationMap;\nexport type PlatformOperationParams<K extends PlatformOperationId> = PlatformOperationMap[K]["params"];\nexport type PlatformOperationResult<K extends PlatformOperationId> = PlatformOperationMap[K]["result"];\nexport const PLATFORM_OPERATIONS = ${JSON.stringify(Object.fromEntries(operations.map(({ operationId, method, path }) => [operationId, { method, path }])), null, 2)} as const;\n`);
  const runtime = JSON.parse(await readFile(new URL("../../../crates/api/contract/http.json", import.meta.url), "utf8"));
  const runtimeIds = new Set(runtime.operations.map(o => o.operationId));
  const runtimeRoutes = new Set(runtime.operations.map(o => `${o.method} ${o.path}`));
  for (const operation of operations) {
    if (runtimeIds.has(operation.operationId) || runtimeRoutes.has(`${operation.method} ${operation.path}`)) throw new Error(`Runtime/native operation collision: ${operation.operationId}`);
  }
  const names = new Set();
  for (const operation of operations) {
    if (!operation.helper?.name || !/^[A-Za-z_$][\w$]*$/.test(operation.helper.name) || names.has(operation.helper.name)) throw new Error(`Invalid or repeated native helper for ${operation.operationId}`);
    names.add(operation.helper.name);
  }
  const helpers = [bannerComment,
    'import type { CallOptions } from "../client.js";',
    'import type { PlatformOperationId, PlatformOperationParams, PlatformOperationResult } from "./platform.js";',
    'interface PlatformCaller { call<K extends PlatformOperationId>(operation: K, params: PlatformOperationParams<K>, options?: CallOptions): Promise<PlatformOperationResult<K>>; }',
    `export const PLATFORM_CLIENT_HELPERS = ${JSON.stringify(Object.fromEntries(operations.map(o => [o.helper.name, o.operationId])), null, 2)} as const;`,
    'export class PlatformHelpers {',
    '  constructor(private readonly client: PlatformCaller) {}',
    ...operations.map(operation => {
      const optional = !(schema.definitions[operation.params].required?.length);
      const id = JSON.stringify(operation.operationId);
      return `  ${operation.helper.name}(params: PlatformOperationParams<${id}>${optional ? ' = {}' : ''}, options: CallOptions = {}): Promise<PlatformOperationResult<${id}>> {
    return this.client.call(${id}, params, options);
  }`;
    }),
    '}', '',
  ];
  await writeFile(new URL("src/generated/platform-helpers.ts", root), helpers.join("\n"));
  await copyFile(new URL("platform.schema.json", contract), new URL("schema/platform.schema.json", root));
  await copyFile(new URL("http.json", contract), new URL("schema/platform-http.json", root));
}
