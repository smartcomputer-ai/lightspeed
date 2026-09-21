import type {
  AuthGrantImportParams,
  ChannelAccountCreateParams,
  ChannelAccountListParams,
  ChannelAccountPutParams,
  ChannelPairingListParams,
} from "@lightspeed-ai/agent-client";
import { Hono } from "hono";
import { z } from "zod";
import {
  readTelegramBotIdentity,
  TelegramConnectionError,
  telegramChannelAccountId,
  whatsAppChannelAccountId,
} from "../channel-connections.js";
import { readChannelsStatus } from "../channels-status.js";
import type { AppContext, ApiVariables } from "../context.js";
import { parseBody } from "../http.js";
import { isPlatformAdmin } from "../context.js";
import { engineClientFor, deploymentClientFor, withGateway } from "./gateway.js";
import { universeForSession } from "./universes.js";

/// Channel accounts are universe resources in the core (`channels/*`): a
/// provider account (a Telegram bot token, a WhatsApp number)
/// belongs to exactly one universe, whose owners/admins manage it. The
/// platform passes through with membership checks, exactly like the bot
/// routes.

async function jsonBody(c: {
  req: { json: () => Promise<unknown> };
}): Promise<Record<string, unknown> | null> {
  try {
    const body = await c.req.json();
    return typeof body === "object" && body !== null
      ? (body as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

const channelConnectionSchema = z.discriminatedUnion("provider", [
  z.object({
    provider: z.literal("telegram"),
    token: z.string().trim().min(1).max(512),
    displayName: z.string().trim().min(1).max(200).optional(),
  }),
  z.object({
    provider: z.literal("whatsapp"),
    phoneNumber: z.string().trim().min(1).max(200),
    displayName: z.string().trim().min(1).max(200).optional(),
    printQr: z.boolean().default(true),
  }),
]);

/// Deployment-wide listing for the admin page and the connector-host
/// operator view: every enabled account across universes, from the
/// core's deployment scope. Platform-admin only.
export function channelAccountAdminRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();

  app.get("/", async (c) => {
    if (!isPlatformAdmin()) {
      return c.json({ error: "platform admin required" }, 403);
    }
    return withGateway(c, async () => {
      const client = deploymentClientFor(ctx);
      const response = await client.call("deployment/channels/accounts/list", {
        includeDisabled: true,
      });
      return c.json(response.result);
    });
  });

  return app;
}

/// Universe-scoped account and pairing routes, mounted under
/// `/universes`.
export function channelUniverseRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();

  app.get("/:id/channel-status", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const statuses = await readChannelsStatus(ctx.env.channelsHealthUrls);
    return c.json({
      accounts: statuses.flatMap((status) =>
        isConnectorHostHealth(status.health)
          ? status.health.accounts.filter(
              (account) => account.universeId === access.universe.lightspeedUniverseId,
            )
          : [],
      ),
    });
  });

  app.get("/:id/channel-accounts", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("channels/accounts/list", {
        provider: c.req.query("provider"),
      } as unknown as ChannelAccountListParams);
      return c.json(response.result);
    });
  });

  app.post("/:id/channel-accounts", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await jsonBody(c);
    const account = body?.account;
    if (!body || typeof account !== "object" || account === null) {
      return c.json({ error: "invalid body" }, 400);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("channels/accounts/create", {
        account,
      } as unknown as ChannelAccountCreateParams);
      return c.json(response.result, 201);
    });
  });

  app.post("/:id/channel-accounts/connect", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, channelConnectionSchema);
    if (!body.ok) {
      return body.response;
    }

    const input = body.data;
    if (input.provider === "whatsapp") {
      const displayName = input.displayName ?? input.phoneNumber;
      return withGateway(c, async () => {
        const client = engineClientFor(ctx, access.universe);
        const response = await client.call("channels/accounts/create", {
          account: {
            accountId: whatsAppChannelAccountId(input.phoneNumber),
            provider: "whatsapp",
            providerAccountId: input.phoneNumber,
            displayName,
            settings: { printQr: input.printQr },
          },
        } as ChannelAccountCreateParams);
        return c.json(response.result, 201);
      });
    }

    let identity;
    try {
      identity = await readTelegramBotIdentity(input.token);
    } catch (error) {
      if (error instanceof TelegramConnectionError) {
        return c.json({ error: error.message }, error.status);
      }
      throw error;
    }
    const accountId = telegramChannelAccountId(identity.username);
    const displayName = input.displayName ?? identity.firstName;
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const grantParams: AuthGrantImportParams = {
        providerId: "telegram",
        exposure: "retrievable",
        token: input.token,
        displayName: `${displayName} Telegram bot token`,
        subjectHint: `@${identity.username}`,
        metadata: { managedBy: "channelAccount", accountId },
      };
      const imported = await client.call("auth/grants/import", grantParams);
      try {
        const response = await client.call("channels/accounts/create", {
          account: {
            accountId,
            provider: "telegram",
            providerAccountId: identity.username,
            displayName,
            credentialGrantId: imported.result.grant.grantId,
            settings: {},
          },
        } as ChannelAccountCreateParams);
        return c.json(response.result, 201);
      } catch (error) {
        await client.call("auth/grants/revoke", {
          grantId: imported.result.grant.grantId,
        }).catch(() => undefined);
        throw error;
      }
    });
  });

  app.get("/:id/channel-accounts/:accountId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("channels/accounts/read", {
        accountId: c.req.param("accountId"),
      });
      return c.json(response.result);
    });
  });

  app.put("/:id/channel-accounts/:accountId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await jsonBody(c);
    const account = body?.account;
    if (!body || typeof account !== "object" || account === null) {
      return c.json({ error: "invalid body" }, 400);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("channels/accounts/put", {
        account: { ...account, accountId: c.req.param("accountId") },
        expectedRevision: body.expectedRevision,
      } as unknown as ChannelAccountPutParams);
      return c.json(response.result);
    });
  });

  app.delete("/:id/channel-accounts/:accountId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("channels/accounts/delete", {
        accountId: c.req.param("accountId"),
      });
      return c.json(response.result);
    });
  });

  app.get("/:id/channel-pairings", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("channels/pairings/list", {
        accountId: c.req.query("accountId"),
        botId: c.req.query("botId"),
      } as unknown as ChannelPairingListParams);
      return c.json(response.result);
    });
  });

  app.delete("/:id/channel-pairings/:accountId/:chatId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("channels/pairings/delete", {
        accountId: c.req.param("accountId"),
        chatId: c.req.param("chatId"),
      });
      return c.json(response.result);
    });
  });

  return app;
}

function isConnectorHostHealth(value: unknown): value is {
  accounts: Array<{ universeId: string } & Record<string, unknown>>;
  [key: string]: unknown;
} {
  return typeof value === "object"
    && value !== null
    && Array.isArray((value as { accounts?: unknown }).accounts);
}
