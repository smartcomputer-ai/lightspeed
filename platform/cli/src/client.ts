import { loadConfig } from "./config.js";

export class ApiError extends Error {
  constructor(
    public status: number,
    public body: unknown,
  ) {
    super(`API error ${status}: ${JSON.stringify(body)}`);
  }
}

export async function api<T = unknown>(
  method: string,
  pathname: string,
  body?: unknown,
): Promise<T> {
  const config = loadConfig();
  if (!config.token) {
    console.error("Not logged in — run `lightspeed-platform login` first.");
    process.exit(1);
  }
  const res = await fetch(new URL(pathname, config.baseUrl), {
    method,
    headers: {
      authorization: `Bearer ${config.token}`,
      // better-auth's CSRF check requires a trusted Origin on mutations.
      origin: new URL(config.baseUrl).origin,
      ...(body !== undefined ? { "content-type": "application/json" } : {}),
    },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  const text = await res.text();
  const json: unknown = text ? JSON.parse(text) : null;
  if (!res.ok) {
    throw new ApiError(res.status, json);
  }
  return json as T;
}

export function printJson(value: unknown): void {
  console.log(JSON.stringify(value, null, 2));
}
