/// Universe channel accounts and pairings: provider accounts are
/// universe resources in the core, so the demo serves the core response
/// shapes under `/universes/:id/channel-accounts` and
/// `/universes/:id/channel-pairings`. The connector-health side effects keep
/// the deployment status page moving like a real deployment's.
import { Hono } from "hono";
import type {
  ChannelAccountView,
  ChannelPairingView,
} from "@lightspeed-ai/sdk";
import { connectorAccountHealth, type ChannelConnectorHealth, type ChannelConnectorStatus } from "@/api";
import type { DemoStore, UniverseState } from "../store";
import { badRequest, conflict, notFound, readBody, universeFor } from "./common";

const CONNECTOR_READY_DELAY_MS = 3_000;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function optionalString(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value.trim() : null;
}

function parseSettings(value: unknown): ChannelAccountView["settings"] {
  return isRecord(value) ? (value as ChannelAccountView["settings"]) : {};
}

function connectorFor(
  store: DemoStore,
  account: ChannelAccountView,
): { connector: ChannelConnectorStatus; health: ChannelConnectorHealth } | undefined {
  for (const connector of store.channelsStatus.connectors) {
    const health = connectorAccountHealth(connector).find(
      (candidate) => candidate.provider === account.provider
        && candidate.accountId === account.providerAccountId,
    );
    if (health) return { connector, health };
  }
  return undefined;
}

export function setConnectorState(
  store: DemoStore,
  account: ChannelAccountView,
  state: NonNullable<ChannelConnectorStatus["health"]>["state"],
): void {
  const match = connectorFor(store, account);
  if (!match) return;
  match.health.state = state as ChannelConnectorHealth["state"];
  match.health.ingressConnected = state === "ready";
  match.health.changedAtMs = Date.now();
}

/// A new account gets a connector that comes up after a beat, so the
/// status column moves like a real deployment's.
function addConnector(store: DemoStore, universe: UniverseState, account: ChannelAccountView): void {
  store.channelsStatus.connectors.push({
    url: `http://channels-${account.provider}-${store.channelsStatus.connectors.length + 1}.internal:9100/health`,
    reachable: true,
    httpStatus: 200,
    health: {
      version: 1,
      provider: account.provider,
      accountId: account.providerAccountId,
      state: account.enabled !== false ? "starting" : "stopped",
      ingressConnected: false,
      activityWorkerReady: true,
      reconnectAttempts: 0,
      changedAtMs: Date.now(),
    },
  });
  if (account.enabled !== false) {
    setTimeout(() => {
      if (universe.channelAccounts.get(account.accountId)?.enabled !== false) {
        setConnectorState(store, account, "ready");
      }
    }, CONNECTOR_READY_DELAY_MS);
  }
}

function removeConnector(store: DemoStore, account: ChannelAccountView): void {
  const match = connectorFor(store, account);
  if (match) {
    store.channelsStatus.connectors.splice(store.channelsStatus.connectors.indexOf(match.connector), 1);
  }
}

/// Reads a `ChannelAccountInput` body into the stored view shape.
function accountFromInput(
  input: Record<string, unknown>,
  existing: ChannelAccountView | undefined,
): ChannelAccountView | string {
  const accountId = optionalString(input.accountId) ?? existing?.accountId ?? null;
  const provider = optionalString(input.provider);
  const providerAccountId = optionalString(input.providerAccountId);
  const displayName = optionalString(input.displayName);
  if (!accountId) return "validation failed — accountId: required";
  if (!provider) return "validation failed — provider: required";
  if (!providerAccountId) return "validation failed — providerAccountId: required";
  if (!displayName) return "validation failed — displayName: required";
  const now = Date.now();
  return {
    accountId,
    provider,
    providerAccountId,
    displayName,
    credentialGrantId: optionalString(input.credentialGrantId),
    settings: input.settings === undefined ? (existing?.settings ?? {}) : parseSettings(input.settings),
    enabled: input.enabled !== false,
    revision: (existing?.revision ?? 0) + 1,
    createdAtMs: existing?.createdAtMs ?? now,
    updatedAtMs: now,
  };
}

export function channelRoutes(store: DemoStore): Hono {
  const app = new Hono();

  app.get("/:id/channel-status", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const accounts = [...universe.channelAccounts.values()];
    return c.json({
      accounts: store.channelsStatus.connectors.flatMap((connector) =>
        connectorAccountHealth(connector).flatMap((health) => {
          const account = accounts.find(
            (candidate) => candidate.provider === health.provider
              && candidate.providerAccountId === health.accountId,
          );
          return account
            ? [{
                ...health,
                universeId: universe.universe.lightspeedUniverseId,
                accountId: account.accountId,
              }]
            : [];
        }),
      ),
    });
  });

  app.get("/:id/channel-accounts", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const provider = c.req.query("provider");
    const accounts = [...universe.channelAccounts.values()].filter(
      (account) => !provider || account.provider === provider,
    );
    return c.json({ accounts });
  });

  app.post("/:id/channel-accounts", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const body = await readBody(c);
    const input = isRecord(body.account) ? body.account : null;
    if (!input) return badRequest(c, "invalid body");
    const account = accountFromInput(input, undefined);
    if (typeof account === "string") return badRequest(c, account);
    if (universe.channelAccounts.has(account.accountId)) {
      return conflict(c, "a channel account with that id already exists");
    }
    universe.channelAccounts.set(account.accountId, account);
    addConnector(store, universe, account);
    return c.json({ account }, 201);
  });

  app.post("/:id/channel-accounts/connect", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const body = await readBody(c);
    const provider = optionalString(body.provider);
    const displayName = optionalString(body.displayName);
    let input: Record<string, unknown>;
    if (provider === "telegram") {
      if (!optionalString(body.token)) return badRequest(c, "validation failed — token: required");
      const sequence = store.nextId("telegram");
      const username = `${sequence.replaceAll("-", "_")}_bot`;
      input = {
        accountId: `telegram-${sequence}`,
        provider,
        providerAccountId: username,
        displayName: displayName ?? "Demo Telegram bot",
        credentialGrantId: store.nextId("authgrant-telegram"),
        settings: {},
      };
    } else if (provider === "whatsapp") {
      const phoneNumber = optionalString(body.phoneNumber);
      if (!phoneNumber) return badRequest(c, "validation failed — phoneNumber: required");
      input = {
        accountId: `whatsapp-${phoneNumber.replace(/[^0-9]+/g, "-").replace(/^-|-$/g, "")}`,
        provider,
        providerAccountId: phoneNumber,
        displayName: displayName ?? phoneNumber,
        settings: { printQr: body.printQr !== false },
      };
    } else {
      return badRequest(c, "validation failed — provider: expected telegram or whatsapp");
    }
    const account = accountFromInput(input, undefined);
    if (typeof account === "string") return badRequest(c, account);
    if (universe.channelAccounts.has(account.accountId)) {
      return conflict(c, "a channel account with that id already exists");
    }
    universe.channelAccounts.set(account.accountId, account);
    addConnector(store, universe, account);
    return c.json({ account }, 201);
  });

  app.get("/:id/channel-accounts/:accountId", (c) => {
    const universe = universeFor(store, c);
    const account = universe?.channelAccounts.get(c.req.param("accountId") ?? "");
    if (!universe || !account) return notFound(c);
    return c.json({ account });
  });

  app.put("/:id/channel-accounts/:accountId", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const accountId = c.req.param("accountId") ?? "";
    const existing = universe.channelAccounts.get(accountId);
    if (!existing) return notFound(c);
    const body = await readBody(c);
    const input = isRecord(body.account) ? body.account : null;
    if (!input) return badRequest(c, "invalid body");
    if (typeof body.expectedRevision === "number" && body.expectedRevision !== existing.revision) {
      return conflict(c, "channel account revision conflict");
    }
    const account = accountFromInput({ ...input, accountId }, existing);
    if (typeof account === "string") return badRequest(c, account);
    universe.channelAccounts.set(accountId, account);
    if ((account.enabled !== false) !== (existing.enabled !== false)) {
      setConnectorState(store, account, account.enabled !== false ? "ready" : "stopped");
    }
    return c.json({ account });
  });

  app.delete("/:id/channel-accounts/:accountId", (c) => {
    const universe = universeFor(store, c);
    const account = universe?.channelAccounts.get(c.req.param("accountId") ?? "");
    if (!universe || !account) return notFound(c);
    universe.channelAccounts.delete(account.accountId);
    universe.channelPairings = universe.channelPairings.filter(
      (pairing) => pairing.accountId !== account.accountId,
    );
    removeConnector(store, account);
    return c.json({ account });
  });

  app.get("/:id/channel-pairings", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const accountId = c.req.query("accountId");
    const botId = c.req.query("botId");
    const pairings = universe.channelPairings.filter(
      (pairing) => (!accountId || pairing.accountId === accountId) && (!botId || pairing.botId === botId),
    );
    return c.json({ pairings });
  });

  app.delete("/:id/channel-pairings/:accountId/:chatId", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const accountId = c.req.param("accountId") ?? "";
    const chatId = c.req.param("chatId") ?? "";
    const index = universe.channelPairings.findIndex(
      (pairing) => pairing.accountId === accountId && pairing.chatId === chatId,
    );
    if (index < 0) return notFound(c);
    const [pairing] = universe.channelPairings.splice(index, 1) as [ChannelPairingView];
    return c.json({ pairing });
  });

  return app;
}
