import { z } from "zod";

/// Input shapes shared by the API (validation) and the CLI (request typing).

export const channelEnum = z.enum(["telegram", "whatsapp"]);

export const channelAccountCreateSchema = z.object({
  provider: channelEnum,
  accountId: z.string().trim().min(1).max(120),
  displayName: z.string().trim().min(1).max(120),
  credentialRef: z.string().trim().min(1).max(240).nullish(),
  stateRef: z.string().trim().min(1).max(240).nullish(),
  settings: z.object({ printQr: z.boolean().optional() }).default({}),
  enabled: z.boolean().optional(),
});
export const channelAccountUpdateSchema = channelAccountCreateSchema.partial().omit({
  provider: true,
  accountId: true,
});

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
    role: z.enum(["owner", "admin", "member"]).default("member"),
  })
  .refine((value) => !!value.userId || !!value.email, {
    message: "userId or email is required",
  });

/// Channel binding scope vocabulary.
export const bindingScopeEnum = z.enum(["direct", "group"]);
export type MemberAddInput = z.infer<typeof memberAddSchema>;

export const channelActivationSchema = z.object({
  group: z.enum(["mention", "always", "silent"]).optional(),
  triggerPrefixes: z.array(z.string().min(1)).optional(),
  mentionNames: z.array(z.string().min(1)).optional(),
  roomContextLimit: z.number().int().min(0).max(1_000).optional(),
  batching: z
    .object({
      debounceMs: z.number().int().min(0).max(5_000).optional(),
      maxWaitMs: z.number().int().min(1).max(30_000).optional(),
      maxMessages: z.number().int().min(1).max(32).optional(),
    })
    .optional(),
});

export const channelAccessSchema = z.object({
  turn: z.enum(["conversation", "members"]).optional(),
  control: z.enum(["none", "members", "admins", "owners"]).optional(),
});

export const bindingCreateSchema = z.object({
  name: z
    .string()
    .regex(/^[a-z0-9][a-z0-9-]*$/)
    .max(60),
  channelAccountId: z.uuid(),
  matchScope: z.enum(["direct", "group"]).nullish(),
  profileId: z.string().min(1).nullish(),
  sessionKey: z.string().min(1),
  activation: channelActivationSchema.nullish(),
  access: channelAccessSchema.nullish(),
  /// Omitted → the server mints one; null → binding pairs implicitly.
  pairingCode: z.string().min(8).nullish(),
  priority: z.number().int().min(0).max(1000).optional(),
  enabled: z.boolean().optional(),
});
export type BindingCreateInput = z.infer<typeof bindingCreateSchema>;

export const bindingUpdateSchema = bindingCreateSchema
  .partial()
  .omit({ name: true });
export type BindingUpdateInput = z.infer<typeof bindingUpdateSchema>;

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

const PAIRING_ALPHABET =
  "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";

/// Mints a pairing code in the same shape as the hand-rolled ones in the old
/// config files (unambiguous alphanumerics, 12 chars).
export function mintPairingCode(length = 12): string {
  const bytes = crypto.getRandomValues(new Uint8Array(length));
  let code = "";
  for (const b of bytes) {
    code += PAIRING_ALPHABET[b % PAIRING_ALPHABET.length];
  }
  return code;
}

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
