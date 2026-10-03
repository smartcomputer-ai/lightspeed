import type { Context } from "hono";
import type { ZodType } from "zod";

/// Parses and validates a JSON body; throws a Response-shaped error the
/// route turns into a 400 with zod issue details.
export async function parseBody<T>(c: Context, schema: ZodType<T>): Promise<
  { ok: true; data: T } | { ok: false; response: Response }
> {
  let raw: unknown;
  try {
    raw = await c.req.json();
  } catch {
    return { ok: false, response: c.json({ error: "invalid JSON body" }, 400) };
  }
  const parsed = schema.safeParse(raw);
  if (!parsed.success) {
    return {
      ok: false,
      response: c.json(
        { error: "validation failed", issues: parsed.error.issues },
        400,
      ),
    };
  }
  return { ok: true, data: parsed.data };
}
