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

/// Member roles, least to most. A universe is an organization; its members
/// hold exactly one of these.
export const UNIVERSE_ROLES = ["viewer", "contributor", "operator", "admin"] as const;
export const universeRoleSchema = z.enum(UNIVERSE_ROLES);
export type UniverseRole = z.infer<typeof universeRoleSchema>;

/// Whether `role` meets `required`.
export function roleAtLeast(role: UniverseRole, required: UniverseRole): boolean {
  return UNIVERSE_ROLES.indexOf(role) >= UNIVERSE_ROLES.indexOf(required);
}

export const memberAddSchema = z
  .object({
    userId: z.string().min(1).optional(),
    email: z.email().optional(),
    role: universeRoleSchema.default("contributor"),
  })
  .refine((value) => !!value.userId || !!value.email, {
    message: "userId or email is required",
  });

export type MemberAddInput = z.infer<typeof memberAddSchema>;

export const memberUpdateSchema = z.object({
  role: universeRoleSchema,
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
