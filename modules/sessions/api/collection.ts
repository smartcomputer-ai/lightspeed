import { z } from "zod";

export const sessionCreateSchema = z.strictObject({
  displayName: z.string().trim().min(1).max(200).optional(),
  metadata: z.record(z.string().min(1).max(64), z.string().min(1).max(256)).optional(),
  deleteAfterCloseMs: z.number().int().positive().nullable().optional(),
  profile: z.discriminatedUnion("kind", [
    z.object({ kind: z.literal("named"), profileId: z.string().min(1) }),
    z.object({ kind: z.literal("inline"), profile: z.looseObject({}) }),
  ]),
  access: z.strictObject({ visibility: z.enum(["universe", "restricted"]) }).optional(),
});

function booleanFilter(value: string | null): boolean | undefined {
  return value === "true" ? true : value === "false" ? false : undefined;
}

/** A repeated metadata filter is either a key or an exact key=value match. */
export function sessionListQuery(url: string) {
  const query = new URL(url).searchParams;
  const metadata: Record<string, string> = Object.create(null);
  for (const raw of query.getAll("metadata")) {
    const at = raw.indexOf("=");
    const key = (at < 0 ? raw : raw.slice(0, at)).trim();
    if (key) metadata[key] = at < 0 ? "" : raw.slice(at + 1).trim();
  }
  const requested = Number(query.get("limit") ?? 50);
  return {
    cursor: query.get("cursor"),
    limit: Number.isSafeInteger(requested) ? Math.min(Math.max(1, requested), 200) : 50,
    closed: booleanFilter(query.get("closed")),
    lifecycleControlled: booleanFilter(query.get("lifecycleControlled")),
    subagent: booleanFilter(query.get("subagent")),
    parent: query.get("parent") || undefined,
    metadata: Object.keys(metadata).length ? metadata : undefined,
  };
}
