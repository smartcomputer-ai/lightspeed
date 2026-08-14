// Regenerates web/src/lib/profile-config-reference.ts from the engine's
// canonical API contract schema (SessionConfig subtree). Run after
// the engine's config schema changes:
//
//   node scripts/generate-config-reference.mjs [path-to-api.schema.json]
//
// The schema ships inside @lightspeed/agent-client (preferred — tracks the
// pinned client version); the sibling-checkout path is the fallback.
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const candidates = [
  path.resolve(here, "../node_modules/@lightspeed/agent-client/schema/api.schema.json"),
  path.resolve(here, "../../../lightspeed/interop/contract/api.schema.json"),
];
const schemaPath = process.argv[2] ?? candidates.find(existsSync);
if (!schemaPath) {
  throw new Error(`api.schema.json not found; looked at:\n${candidates.join("\n")}`);
}
const outPath = path.resolve(here, "../web/src/lib/profile-config-reference.ts");

const schema = JSON.parse(readFileSync(schemaPath, "utf8"));
const defs = schema.definitions;

const deref = (node) =>
  node.$ref ? defs[node.$ref.split("/").pop()] : node;

/// Unwraps `anyOf: [X, null]` / `type: [T, "null"]` to the inner shape.
function unwrapNullable(node) {
  if (node.anyOf) {
    const inner = node.anyOf.filter((n) => n.type !== "null");
    if (inner.length === 1) {
      return inner[0];
    }
  }
  if (Array.isArray(node.type)) {
    const types = node.type.filter((t) => t !== "null");
    if (types.length === 1) {
      return { ...node, type: types[0] };
    }
  }
  return node;
}

/// Single-line rendering for tagged-union variants.
function compactHint(node) {
  node = unwrapNullable(deref(unwrapNullable(node)));
  if (node.const !== undefined) {
    return JSON.stringify(node.const);
  }
  if (node.enum) {
    return node.enum.map((v) => JSON.stringify(v)).join(" | ");
  }
  if (node.type === "string") {
    return '"string"';
  }
  if (node.type === "integer" || node.type === "number") {
    return "0";
  }
  if (node.type === "boolean") {
    return "true | false";
  }
  if (node.properties) {
    const fields = Object.entries(node.properties).map(
      ([name, prop]) => `"${name}": ${compactHint(prop)}`,
    );
    return `{ ${fields.join(", ")} }`;
  }
  return '"…"';
}

function valueHint(node, indent, stack) {
  node = unwrapNullable(deref(unwrapNullable(node)));
  if (node.const !== undefined) {
    return JSON.stringify(node.const);
  }
  if (node.oneOf) {
    // Tagged union: one compact variant per line.
    const pad = "  ".repeat(indent + 1);
    const variants = node.oneOf.map((arm) => `${pad}${compactHint(arm)}`);
    return `// one of:\n${variants.join(" |\n")}`;
  }
  if (node.enum) {
    return node.enum.map((v) => JSON.stringify(v)).join(" | ");
  }
  if (node.type === "string") {
    return '"string"';
  }
  if (node.type === "integer" || node.type === "number") {
    return "0";
  }
  if (node.type === "boolean") {
    return "true | false";
  }
  if (node.type === "array") {
    return `[${valueHint(node.items ?? {}, indent, stack)}]`;
  }
  if (node.properties || node.type === "object") {
    return renderObject(node, indent, stack);
  }
  if (node.anyOf) {
    // Non-null union (e.g. tagged variants): render each arm.
    return node.anyOf.map((arm) => valueHint(arm, indent, stack)).join(" | ");
  }
  return '"…"';
}

function renderObject(node, indent, stack) {
  const properties = node.properties ?? {};
  const pad = "  ".repeat(indent + 1);
  const lines = ["{"];
  for (const [name, prop] of Object.entries(properties)) {
    const required = (node.required ?? []).includes(name);
    const resolved = deref(unwrapNullable(prop));
    const description = (prop.description ?? resolved.description ?? "")
      .replace(/\s+/g, " ")
      .trim();
    if (description) {
      lines.push(`${pad}// ${description}`);
    }
    if (required) {
      lines.push(`${pad}// (required when this object is present)`);
    }
    const refName = (prop.$ref ?? unwrapNullable(prop).$ref)?.split("/").pop();
    if (refName && stack.includes(refName)) {
      lines.push(`${pad}"${name}": { /* ${refName} */ },`);
      continue;
    }
    const hint = valueHint(prop, indent + 1, refName ? [...stack, refName] : stack);
    lines.push(`${pad}"${name}": ${hint},`);
  }
  if (node.additionalProperties && typeof node.additionalProperties === "object") {
    lines.push(
      `${pad}"<key>": ${valueHint(node.additionalProperties, indent + 1, stack)},`,
    );
  }
  lines.push(`${"  ".repeat(indent)}}`);
  return lines.join("\n");
}

const rendered = renderObject(defs.SessionConfig, 0, ["SessionConfig"]);
const banner = [
  "/// GENERATED — do not edit by hand.",
  "/// Source: lightspeed interop/contract/api.schema.json (SessionConfig).",
  "/// Regenerate with: node scripts/generate-config-reference.mjs",
  "",
  "export const PROFILE_CONFIG_REFERENCE = `// Every field is optional — omit anything to keep engine defaults.",
  "// Union values are written a | b — pick one.",
].join("\n");

writeFileSync(outPath, `${banner}\n${rendered.replaceAll("`", "\\`")}\n\`;\n`);
console.log(`wrote ${outPath}`);
