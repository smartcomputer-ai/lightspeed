import { z } from "zod";

/// Input shapes shared by the API (validation) and the CLI (request typing).

export const universeCreateSchema = z.object({
  name: z.string().min(1).max(100),
  /// URL-safe org slug; derived from name when omitted.
  slug: z
    .string()
    .regex(/^[a-z0-9][a-z0-9-]*$/)
    .max(60)
    .optional(),
});
export type UniverseCreateInput = z.infer<typeof universeCreateSchema>;

export const universeUpdateSchema = z.object({
  name: z.string().min(1).max(100).optional(),
  gatewayUrl: z.union([z.url(), z.null()]).optional(),
  status: z.enum(["active", "archived"]).optional(),
});
export type UniverseUpdateInput = z.infer<typeof universeUpdateSchema>;

export const memberAddSchema = z
  .object({
    userId: z.string().min(1).optional(),
    email: z.email().optional(),
    groupId: z.string().uuid().optional(),
    role: z.enum(["viewer", "contributor", "operator", "admin"]).default("contributor"),
  })
  .refine((value) => !!value.userId || !!value.email || !!value.groupId, {
    message: "userId, email or groupId is required",
  });

export type MemberAddInput = z.infer<typeof memberAddSchema>;

export const memberUpdateSchema = z.object({
  role: z.enum(["viewer", "contributor", "operator", "admin"]),
});
export type MemberUpdateInput = z.infer<typeof memberUpdateSchema>;

export const workspaceCreateSchema = z.object({
  /// Gateway workspace id; minted from the display name (or randomly) when
  /// omitted.
  workspaceId: z
    .string()
    .regex(/^[a-z0-9][a-z0-9._-]*$/)
    .max(80)
    .optional(),
  displayName: z.string().min(1).max(100).optional(),
});
export type WorkspaceCreateInput = z.infer<typeof workspaceCreateSchema>;


export function slugify(name: string): string {
  return (
    name
      .toLowerCase()
      .normalize("NFKD")
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 60) || "universe"
  );
}
