import { z } from "zod";
import { universeIconSchema, universeIconColorSchema } from "./universe-appearance.js";
export * from "./universe-appearance.js";
export * from "./workspace-transfers.js";

/// Input shapes shared by the API (validation) and the CLI (request typing).

/// A universe's handle is its deployment-unique name; the UUID remains stable
/// on rename. The runtime owns the rule, mirrored here so forms can check
/// before sending: a lowercase DNS label that is not in the form of a UUID.
export const UNIVERSE_HANDLE_MAX_LENGTH = 63;
const UUID_SHAPED = /^(?:[0-9a-f]{32}|[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12})$/;
export const universeHandleSchema = z
  .string()
  .regex(/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/)
  .refine((handle) => !UUID_SHAPED.test(handle));

export const universeCreateSchema = z.object({
  name: z.string().min(1).max(100),
  /// Requested handle; derived from the name when omitted.
  handle: universeHandleSchema.optional(),
});
export type UniverseCreateInput = z.infer<typeof universeCreateSchema>;

export const universeUpdateSchema = z.object({
  name: z.string().min(1).max(100).optional(),
  icon: universeIconSchema.optional(),
  iconColor: universeIconColorSchema.optional(),
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

export const workspaceRenameSchema = z.object({
  displayName: z.string().trim().min(1).max(100),
  expectedRevision: z.number().int().nonnegative(),
});


export function slugify(name: string): string {
  return (
    name
      .toLowerCase()
      .normalize("NFKD")
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 60)
      // The cut can land on a separator.
      .replace(/-+$/, "") || "universe"
  );
}

/// The handle suggested for a universe's display name. A name that reduces to
/// the form of a UUID is prefixed so it stays a valid handle.
export function universeHandleFromName(name: string): string {
  const handle = slugify(name);
  return UUID_SHAPED.test(handle) ? `u-${handle}`.slice(0, UNIVERSE_HANDLE_MAX_LENGTH) : handle;
}

/// The id of an API key or registration key when its creator chooses it
/// instead of having one allocated. The runtime owns which characters it may
/// hold and refuses the rest as an invalid request.
export const KEY_ID_MAX_LENGTH = 128;
export const chosenKeyIdSchema = z.string().trim().min(1).max(KEY_ID_MAX_LENGTH);

/// Why a create that chose its own key id was refused: the id is taken. A
/// revoked key keeps its id for good, so that case reads differently.
export function keyIdConflictMessage(keyId: string, revoked: boolean): string {
  return revoked
    ? `key ID "${keyId}" belongs to a revoked key and cannot be reused; leave it blank to generate a new ID or choose another`
    : `key ID "${keyId}" is already in use; leave it blank to generate a new ID or choose another`;
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

/// Parts of the product a universe admin can switch on or off. A feature that
/// is off is hidden in the web app and its runtime methods are refused for
/// every member (`FEATURE_METHOD_PREFIXES`). The switch governs what members
/// do through the Platform: what already runs in the runtime (a bot answering
/// on its channels) keeps running, and callers holding a runtime key are not
/// affected. Features that let agents act on their own start switched off.
/// Add a feature here and every surface can ask for it by key.
export const FEATURES = defineFeatures({
  bots: {
    label: "Bots",
    description: "Agents that run on their own: they react to events, schedules and chats.",
    default: false,
    requires: [],
  },
  channels: {
    label: "Channels",
    description: "Chat accounts, such as Telegram, that bots talk through.",
    default: false,
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

/// The switches a universe stored, read one at a time. An entry this version
/// does not know, or one that is not a switch, is skipped on its own: with
/// features off by default, one stray entry must not switch the others off.
function storedOverrides(stored: unknown): FeatureOverrides {
  const entries = typeof stored === "object" && stored !== null ? (stored as Record<string, unknown>) : {};
  const overrides: FeatureOverrides = {};
  for (const key of FEATURE_KEYS) {
    const value = entries[key];
    if (typeof value === "boolean") overrides[key] = value;
  }
  return overrides;
}

/// Whether each feature is on: its own switch (or default), and every
/// feature it requires on too. A feature whose requirement is off keeps its
/// own switch, so turning the requirement back on restores it.
export function effectiveFeatures(stored: unknown): FeatureStates {
  const overrides = storedOverrides(stored);
  const own = (key: FeatureKey) => overrides[key] ?? FEATURES[key].default;
  const on = (key: FeatureKey): boolean =>
    own(key) && (FEATURES[key].requires as readonly FeatureKey[]).every(on);
  return Object.fromEntries(FEATURE_KEYS.map((key) => [key, on(key)])) as FeatureStates;
}

/// The overrides after `changes`: a switch set back to its default is
/// dropped, so a universe stores only what differs.
export function mergeFeatureOverrides(stored: unknown, changes: FeatureOverrides): FeatureOverrides {
  const merged: FeatureOverrides = { ...storedOverrides(stored), ...changes };
  for (const key of FEATURE_KEYS) {
    if (merged[key] === FEATURES[key].default) delete merged[key];
  }
  return merged;
}

/// The runtime methods that belong to a feature, by the prefix of their
/// name. A member's call to one is refused while the feature is off.
export const FEATURE_METHOD_PREFIXES: Record<FeatureKey, readonly string[]> = {
  bots: ["bots/"],
  channels: ["channels/"],
};

/// The feature a runtime method belongs to, if any.
export function featureOfMethod(method: string): FeatureKey | null {
  return FEATURE_KEYS.find((key) => FEATURE_METHOD_PREFIXES[key].some((prefix) => method.startsWith(prefix))) ?? null;
}
export { AGENT_MODEL_API_KINDS, modelDefaultsPutSchema } from "./model-defaults.js";
export { MAX_DICTATION_AUDIO_BYTES, MAX_DICTATION_SECONDS, transcriptionStartSchema, transcriptionUploadSchema } from "./transcriptions.js";
export {
  ATTACHMENT_ACCEPT,
  ATTACHMENT_MIMES,
  ATTACHMENT_SUMMARY,
  MAX_ANTHROPIC_IMAGE_BYTES,
  MAX_ATTACHMENT_BYTES,
  MAX_MESSAGE_ATTACHMENTS,
  attachmentLimit,
  attachmentType,
  attachmentUploadSchema,
  messageAttachmentSchema,
  messageInputItems,
  messageRunConfig,
  messageRunOptionsSchema,
  reasoningEffortTiers,
  sessionMessageSchema,
  sessionSteerSchema,
  type AttachmentKind,
  type AttachmentType,
  type MessageAttachment,
  type MessageRunOptions,
} from "./messages.js";

/** Closing shared work is an operator responsibility, even for its creator. */
export function canCloseSession(role: UniverseRole | null | undefined, creator: boolean, shared: boolean): boolean {
  return role === "admin" || (role === "operator" && (creator || shared)) ||
    (role === "contributor" && creator && !shared);
}

/** Operators may remove shared sessions; private sessions require an admin. */
export function canDeleteSession(role: UniverseRole | null | undefined, shared: boolean): boolean {
  return role === "admin" || (role === "operator" && shared);
}
