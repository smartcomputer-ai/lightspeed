import { and, asc, eq } from "drizzle-orm";
import { schema, type Db } from "@lightspeed/platform-db";
import type { ChannelProvider, NormalizedInboundV1 } from "../contracts/channel.js";
import type { ChannelRoute } from "../contracts/channel.js";
import { channelPairingKey } from "../identity/ids.js";
import {
  resolveActivationSettings,
  type ChannelActivationSettings,
} from "../policy/activation.js";
import {
  authorizeChannelSender,
  resolveAccessSettings,
  type ChannelAccessSettings,
  type ChannelAuthorization,
  type ChannelMemberRole,
} from "../policy/access.js";

export interface ChannelBindingCandidate {
  bindingId: string;
  bindingName: string;
  channelAccountId: string;
  accountProvider: ChannelProvider;
  accountId: string;
  universeId: string;
  universeName: string;
  universeActive: boolean;
  enabled: boolean;
  matchScope: "direct" | "group" | null;
  profileId: string | null;
  sessionKey: string;
  pairingRequired: boolean;
  pairingCode: string | null;
  paired: boolean;
  activation: unknown;
  access: unknown;
  memberRole: ChannelMemberRole;
}

export interface ResolvedChannelBinding {
  bindingId: string;
  bindingName: string;
  universeId: string;
  universeName: string;
  profileId: string | null;
  sessionKey: string;
  activation: ChannelActivationSettings;
  access: ChannelAccessSettings;
  authorization: ChannelAuthorization;
}

export interface ChannelBindingResolver {
  resolve(inbound: NormalizedInboundV1): Promise<ResolvedChannelBinding | null>;
}

export type ChannelAdmissionDecision =
  | { status: "bound"; binding: ResolvedChannelBinding }
  | { status: "paired"; binding: ResolvedChannelBinding }
  | { status: "pairing_required" }
  | { status: "pairing_pending" }
  | { status: "unbound" };

export interface ChannelControlPlane extends ChannelBindingResolver {
  admit(inbound: NormalizedInboundV1): Promise<ChannelAdmissionDecision>;
  pairingRequired(route: ChannelRoute, scope: "direct" | "group"): Promise<boolean>;
}

export type ChannelAdmissionPlan =
  | { status: "bound"; binding: ResolvedChannelBinding }
  | { status: "pair"; candidate: ChannelBindingCandidate }
  | { status: "pairing_required" }
  | { status: "pairing_pending" }
  | { status: "unbound" };

export function selectChannelBinding(
  candidates: readonly ChannelBindingCandidate[],
  route: ChannelRoute,
  isDirect: boolean,
): ResolvedChannelBinding | null {
  const scope = isDirect ? "direct" : "group";
  const candidate = candidates.find(
    (row) =>
      row.enabled &&
      row.universeActive &&
      row.accountProvider === route.provider &&
      row.accountId === route.accountId &&
      (row.matchScope === null || row.matchScope === scope) &&
      (!row.pairingRequired || row.paired),
  );
  if (candidate === undefined) {
    return null;
  }
  const access = resolveAccessSettings(candidate.access);
  return {
    bindingId: candidate.bindingId,
    bindingName: candidate.bindingName,
    universeId: candidate.universeId,
    universeName: candidate.universeName,
    profileId: candidate.profileId,
    sessionKey: candidate.sessionKey,
    activation: resolveActivationSettings(scope, candidate.activation),
    access,
    authorization: authorizeChannelSender(access, candidate.memberRole),
  };
}

export function planChannelAdmission(
  candidates: readonly ChannelBindingCandidate[],
  inbound: NormalizedInboundV1,
): ChannelAdmissionPlan {
  const relevant = matchingCandidates(candidates, inbound.route, inbound.isDirect);
  const pairable: ChannelBindingCandidate[] = [];
  for (const candidate of relevant) {
    if (!candidate.pairingRequired) {
      if (pairable.length === 0) {
        return { status: "bound", binding: resolveCandidate(candidate, inbound.isDirect) };
      }
      break;
    }
    pairable.push(candidate);
  }
  const alreadyPaired = pairable.find((candidate) => candidate.paired);
  if (alreadyPaired !== undefined) {
    return { status: "bound", binding: resolveCandidate(alreadyPaired, inbound.isDirect) };
  }
  const code = inbound.text.trim();
  const matched = code.length === 0
    ? undefined
    : pairable.find((candidate) => candidate.pairingCode === code);
  if (matched !== undefined) {
    return { status: "pair", candidate: matched };
  }
  if (pairable.length === 0) {
    return { status: "unbound" };
  }
  return shouldPromptForPairing(inbound, pairable)
    ? { status: "pairing_required" }
    : { status: "pairing_pending" };
}

export function createDbBindingResolver(db: Db): ChannelBindingResolver {
  return createDbChannelControlPlane(db);
}

export function createDbChannelControlPlane(db: Db): ChannelControlPlane {
  return {
    async resolve(inbound) {
      const candidates = await readCandidates(db, inbound);
      return selectChannelBinding(candidates, inbound.route, inbound.isDirect);
    },
    async admit(inbound) {
      const plan = planChannelAdmission(await readCandidates(db, inbound), inbound);
      if (plan.status !== "pair") {
        return plan;
      }
      await db
        .insert(schema.pairings)
        .values({
          key: channelPairingKey(inbound.route),
          bindingId: plan.candidate.bindingId,
          channelAccountId: plan.candidate.channelAccountId,
          chatId: inbound.route.chatId,
        })
        .onConflictDoUpdate({
          target: schema.pairings.key,
          set: {
            bindingId: plan.candidate.bindingId,
            channelAccountId: plan.candidate.channelAccountId,
            chatId: inbound.route.chatId,
            updatedAt: new Date(),
          },
        });
      return {
        status: "paired",
        binding: resolveCandidate({ ...plan.candidate, paired: true }, inbound.isDirect),
      };
    },
    async pairingRequired(route, scope) {
      const inbound: NormalizedInboundV1 = {
        version: 1,
        messageId: "pairing-probe",
        route,
        senderId: "pairing-probe",
        senderName: "pairing-probe",
        timestampMs: 0,
        text: "",
        isDirect: scope === "direct",
        mentionedBot: false,
        isReplyToBot: false,
      };
      const plan = planChannelAdmission(await readCandidates(db, inbound), inbound);
      return plan.status === "pairing_pending" || plan.status === "pairing_required";
    },
  };
}

async function readCandidates(
  db: Db,
  inbound: NormalizedInboundV1,
): Promise<ChannelBindingCandidate[]> {
  const route = inbound.route;
  const rows = await db
    .select({
      bindingId: schema.bindings.id,
      bindingName: schema.bindings.name,
      channelAccountId: schema.channelAccounts.id,
      accountProvider: schema.channelAccounts.provider,
      accountId: schema.channelAccounts.accountId,
      universeId: schema.universes.lightspeedUniverseId,
      universeName: schema.universes.name,
      universeStatus: schema.universes.status,
      enabled: schema.bindings.enabled,
      matchScope: schema.bindings.matchScope,
      profileId: schema.bindings.profileId,
      sessionKey: schema.bindings.sessionKey,
      pairingCode: schema.bindings.pairingCode,
      pairingKey: schema.pairings.key,
      activation: schema.bindings.activation,
      access: schema.bindings.access,
      memberRole: schema.member.role,
    })
    .from(schema.bindings)
    .innerJoin(schema.universes, eq(schema.universes.id, schema.bindings.universeId))
    .innerJoin(
      schema.channelAccounts,
      and(
        eq(schema.channelAccounts.id, schema.bindings.channelAccountId),
        eq(schema.channelAccounts.provider, route.provider),
        eq(schema.channelAccounts.accountId, route.accountId),
        eq(schema.channelAccounts.enabled, true),
      ),
    )
    .leftJoin(
      schema.pairings,
      and(
        eq(schema.pairings.bindingId, schema.bindings.id),
        eq(schema.pairings.channelAccountId, schema.channelAccounts.id),
        eq(schema.pairings.chatId, route.chatId),
      ),
    )
    .leftJoin(
      schema.channelIdentities,
      and(
        eq(schema.channelIdentities.channel, route.provider),
        eq(schema.channelIdentities.handle, inbound.senderId),
      ),
    )
    .leftJoin(
      schema.member,
      and(
        eq(schema.member.userId, schema.channelIdentities.userId),
        eq(schema.member.organizationId, schema.universes.organizationId),
      ),
    )
    .where(eq(schema.bindings.enabled, true))
    .orderBy(asc(schema.bindings.priority), asc(schema.bindings.createdAt));
  return rows.map((row) => ({
    bindingId: row.bindingId,
    bindingName: row.bindingName,
    channelAccountId: row.channelAccountId,
    accountProvider: row.accountProvider,
    accountId: row.accountId,
    universeId: row.universeId,
    universeName: row.universeName,
    universeActive: row.universeStatus === "active",
    enabled: row.enabled,
    matchScope: row.matchScope,
    profileId: row.profileId,
    sessionKey: row.sessionKey,
    pairingRequired: row.pairingCode !== null,
    pairingCode: row.pairingCode,
    paired: row.pairingKey !== null,
    activation: row.activation,
    access: row.access,
    memberRole: memberRole(row.memberRole),
  }));
}

function matchingCandidates(
  candidates: readonly ChannelBindingCandidate[],
  route: ChannelRoute,
  isDirect: boolean,
): ChannelBindingCandidate[] {
  const scope = isDirect ? "direct" : "group";
  return candidates.filter(
    (row) =>
      row.enabled &&
      row.universeActive &&
      row.accountProvider === route.provider &&
      row.accountId === route.accountId &&
      (row.matchScope === null || row.matchScope === scope),
  );
}

function resolveCandidate(
  candidate: ChannelBindingCandidate,
  isDirect: boolean,
): ResolvedChannelBinding {
  const scope = isDirect ? "direct" : "group";
  const access = resolveAccessSettings(candidate.access);
  return {
    bindingId: candidate.bindingId,
    bindingName: candidate.bindingName,
    universeId: candidate.universeId,
    universeName: candidate.universeName,
    profileId: candidate.profileId,
    sessionKey: candidate.sessionKey,
    activation: resolveActivationSettings(scope, candidate.activation),
    access,
    authorization: authorizeChannelSender(access, candidate.memberRole),
  };
}

function shouldPromptForPairing(
  inbound: NormalizedInboundV1,
  candidates: readonly ChannelBindingCandidate[],
): boolean {
  if (inbound.isDirect || inbound.mentionedBot || inbound.isReplyToBot) {
    return true;
  }
  const text = inbound.text.trim().toLowerCase();
  return candidates.some((candidate) =>
    resolveActivationSettings("group", candidate.activation).triggerPrefixes.some((prefix) => {
      const normalized = prefix.toLowerCase();
      return text === normalized || text.startsWith(`${normalized} `) || text.startsWith(`${normalized}@`);
    }),
  );
}

function memberRole(value: string | null): ChannelMemberRole {
  return value === "member" || value === "admin" || value === "owner" ? value : null;
}
