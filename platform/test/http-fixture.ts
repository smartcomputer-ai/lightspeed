import { HTTP_OPERATIONS } from "@lightspeed-ai/sdk/server";

/** Decode a real resource request for transport-level consumer tests. */
export function decodeHttpRequest(input: string | Request | URL, init?: RequestInit) {
  const url = new URL(input instanceof Request ? input.url : String(input));
  const method = init?.method ?? (input instanceof Request ? input.method : "GET");
  const specs = Object.values(HTTP_OPERATIONS).sort((a, b) => b.path.replace(/\{[^}]+\}/g, "").length - a.path.replace(/\{[^}]+\}/g, "").length);
  for (const spec of specs) {
    if (spec.method !== method) continue;
    const names: string[] = [];
    const pattern = spec.path.replace(/[.*+?^${}()|[\]\\]/g, char => char === "{" || char === "}" ? char : `\\${char}`)
      .replace(/\{([^}]+)\}/g, (_match, name: string) => { names.push(name); return "([^/]+)"; });
    const matched = new RegExp(`^${pattern}$`).exec(url.pathname);
    if (!matched) continue;
    const path = Object.fromEntries(names.map((name, i) => [name, decodeURIComponent(matched[i + 1]!)]));
    const params: Record<string, any> = init?.body ? JSON.parse(String(init.body)) as Record<string, unknown> : {};
    for (const field of spec.pathFields) {
      const parameter = spec.parameters.find(parameter => parameter.in === "path" && parameter.name === field.parameter);
      const schema = (parameter && "schema" in parameter ? parameter.schema : {}) as { type?: string };
      const raw = path[field.parameter];
      setField(params, field.field, schema.type === "integer" || schema.type === "number" ? Number(raw) : raw);
    }
    for (const parameter of spec.parameters) {
      if (parameter.in !== "query" || !url.searchParams.has(parameter.name)) continue;
      const raw = url.searchParams.get(parameter.name)!;
      const encoding = "x-lightspeed-encoding" in parameter ? parameter["x-lightspeed-encoding"] : "scalar";
      const schema = ("schema" in parameter ? parameter.schema : {}) as { type?: string | readonly string[] };
      const types = Array.isArray(schema.type) ? schema.type : [schema.type];
      const value = encoding === "repeated" ? url.searchParams.getAll(parameter.name) : encoding === "json" ? JSON.parse(raw) : types.includes("boolean") ? raw === "true" : types.some(type => type === "integer" || type === "number") ? Number(raw) : raw;
      setField(params, parameter.name, value);
    }
    return { operation: spec.operationId, method: spec.operationId, params, universe: path.universe, headers: new Headers(init?.headers), url };
  }
  throw new Error(`Unrecognized runtime HTTP request: ${method} ${url.pathname}`);
}

function setField(object: Record<string, unknown>, field: string, value: unknown) {
  const parts = field.split("."); const key = parts.pop()!;
  for (const part of parts) object = (object[part] ??= {}) as Record<string, unknown>;
  object[key] = value;
}

/** Return a structured HTTP failure from a runtime fixture. */
export function problemResponse(problem: { status: number; kind: string; detail: string; [key: string]: unknown }): Response {
  return Response.json({ type: "about:blank", title: problem.detail, ...problem }, {
    status: problem.status, headers: { "content-type": "application/problem+json" },
  });
}
