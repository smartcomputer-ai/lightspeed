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
  /// Switches to change; others keep what they were.
  features: z.lazy(() => featureOverridesSchema).optional(),
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

interface FeatureDefinition<Key extends string> {
  label: string;
  description: string;
  default: boolean;
  /// Features that must be on for this one to be.
  requires: readonly Key[];
}

/// Checks every `requires` against the registry's own keys.
function defineFeatures<const Features extends Record<string, FeatureDefinition<Extract<keyof Features, string>>>>(
  features: Features,
): Features {
  return features;
}

/// Parts of the product a universe admin can switch on or off. Switching one
/// off hides it from the web app; the API behind it keeps working, and what
/// already runs (a bot answering on its channels) keeps running. Add a
/// feature here and every surface can ask for it by key.
export const FEATURES = defineFeatures({
  bots: {
    label: "Bots",
    description: "Agents that run on their own: they react to events, schedules and chats.",
    default: true,
    requires: [],
  },
  channels: {
    label: "Channels",
    description: "Chat accounts, such as Telegram, that bots talk through.",
    default: true,
    requires: ["bots"],
  },
});
export type FeatureKey = keyof typeof FEATURES;
export const FEATURE_KEYS = Object.keys(FEATURES) as FeatureKey[];

/// What a universe stored: only switches set away from their default.
export type FeatureOverrides = Partial<Record<FeatureKey, boolean>>;
/// Every feature, on or off, after defaults and what each requires.
export type FeatureStates = Record<FeatureKey, boolean>;

export const featureOverridesSchema = z.partialRecord(z.enum(FEATURE_KEYS as [FeatureKey, ...FeatureKey[]]), z.boolean());

/// Whether each feature is on: its own switch (or default), and every
/// feature it requires on too. A feature whose requirement is off keeps its
/// own switch, so turning the requirement back on restores it.
export function effectiveFeatures(stored: unknown): FeatureStates {
  const overrides = featureOverridesSchema.safeParse(stored ?? {}).data ?? {};
  const own = (key: FeatureKey) => overrides[key] ?? FEATURES[key].default;
  const on = (key: FeatureKey): boolean =>
    own(key) && (FEATURES[key].requires as readonly FeatureKey[]).every(on);
  return Object.fromEntries(FEATURE_KEYS.map((key) => [key, on(key)])) as FeatureStates;
}

/// The overrides after `changes`: a switch set back to its default is
/// dropped, so a universe stores only what differs.
export function mergeFeatureOverrides(stored: unknown, changes: FeatureOverrides): FeatureOverrides {
  const merged: FeatureOverrides = { ...(featureOverridesSchema.safeParse(stored ?? {}).data ?? {}), ...changes };
  for (const key of FEATURE_KEYS) {
    if (merged[key] === FEATURES[key].default) delete merged[key];
  }
  return merged;
}
