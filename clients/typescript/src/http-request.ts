import { HTTP_OPERATIONS } from "./generated/http.js";
import type { OperationId, OperationParams } from "./generated/operations.js";

interface Binding {
  method: string;
  path: string;
  transfer: string;
  pathFields: readonly { field: string; parameter: string }[];
  parameters: readonly { name: string; in: string; "x-lightspeed-encoding"?: string }[];
}

/** Partition logical operation parameters according to the generated HTTP binding. */
export function prepareHttpRequest<M extends OperationId>(
  operation: M,
  params: OperationParams<M>,
  options: { endpoint: string | URL; universe?: string },
): { url: URL; method: string; body?: Record<string, unknown> } {
  const binding: Binding = HTTP_OPERATIONS[operation];
  const remaining = structuredClone(params) as Record<string, unknown>;
  let pathname = binding.path;
  for (const { field, parameter } of binding.pathFields) {
    const value = take(remaining, field);
    pathname = pathname.replace(`{${parameter}}`, segment(value, field));
  }
  if (pathname.includes("{universe}")) {
    pathname = pathname.replace("{universe}", segment(options.universe, "universe"));
  }
  const endpoint = new URL(options.endpoint);
  if (endpoint.search || endpoint.hash) throw new TypeError("API endpoint must not contain a query or fragment");
  const url = new URL(endpoint.toString().replace(/\/$/, "") + pathname.slice("/api/v1".length));
  for (const parameter of binding.parameters) {
    if (parameter.in !== "query") continue;
    const value = take(remaining, parameter.name);
    if (value === undefined || value === null) continue;
    switch (parameter["x-lightspeed-encoding"]) {
      case "repeated":
        if (!Array.isArray(value)) throw new TypeError(`${parameter.name} must be an array`);
        for (const item of value) url.searchParams.append(parameter.name, String(item));
        break;
      case "json": url.searchParams.set(parameter.name, JSON.stringify(value)); break;
      default: url.searchParams.set(parameter.name, String(value));
    }
  }
  const body = binding.method === "GET" || binding.method === "HEAD" || binding.transfer === "binaryUpload"
    ? undefined : remaining;
  if (!body && Object.keys(remaining).length) {
    throw new TypeError(`Unexpected parameters for ${operation}: ${Object.keys(remaining).join(", ")}`);
  }
  return body === undefined ? { url, method: binding.method } : { url, method: binding.method, body };
}

function segment(value: unknown, name: string): string {
  if (typeof value === "number" && Number.isSafeInteger(value)) return String(value);
  if (typeof value !== "string" || !value || value === "." || value === "..") {
    throw new TypeError(`${name} must be a nonempty path identifier`);
  }
  return encodeURIComponent(value);
}

function take(object: Record<string, unknown>, field: string): unknown {
  const parts = field.split(".");
  const last = parts.pop()!;
  let owner = object;
  for (const part of parts) {
    const nested = owner[part];
    if (!nested || typeof nested !== "object" || Array.isArray(nested)) return undefined;
    owner = nested as Record<string, unknown>;
  }
  const value = owner[last];
  delete owner[last];
  return value;
}
