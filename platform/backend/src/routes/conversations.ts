import { Hono, type Context } from "hono";
import { z } from "zod";
import { LightspeedApiError, type SessionConfig } from "@lightspeed-ai/sdk";
import { messageInputItems, messageRunConfig, sessionMessageSchema, sessionSteerSchema } from "@lightspeed-ai/platform-shared";
import type { AppContext, ApiVariables } from "../context.js";
import { parseBody, withApiErrors } from "../http.js";
import { universeClient, type UniverseClient } from "../runtime-client.js";
import { universeForRequest } from "./universes.js";

/// Put replaces the whole map; an empty map clears it.
const sessionMetadataPutSchema = z.object({
  metadata: z.record(z.string().min(1).max(64), z.string().min(1).max(256)).default({}),
});

const sessionConfigPutSchema = z.object({
  config: z.looseObject({}),
  expectedConfigRevision: z.number().int().min(0),
});

const sessionCloseSchema = z.object({
  force: z.boolean().optional(),
});

const sessionRetentionPutSchema = z.object({
  deleteAfterCloseMs: z.number().int().positive().nullable(),
});

const sessionInstructionsPutSchema = z.object({
  text: z.string().max(1_000_000).nullable(),
});

const sessionApprovalDecideSchema = z.object({
  decisions: z
    .array(
      z.object({
        approvalId: z.string().regex(/^approval_[1-9][0-9]*$/),
        decision: z.enum(["approve", "reject"]),
        note: z.string().trim().min(1).max(2_000).optional(),
      }),
    )
    .min(1)
    .max(64),
});

/** Member-scoped operations on a conversation embedded in any platform surface. */
export function conversationRoutes(ctx: AppContext) {
  const base = "/:id/sessions/:sessionId";
  const app = new Hono<{ Variables: ApiVariables }>().basePath(base);

  async function withConversation(
    c: Context<{ Variables: ApiVariables }, typeof base>,
    handle: (client: UniverseClient) => Promise<Response>,
  ): Promise<Response> {
    const access = await universeForRequest(ctx, c, c.req.param("id"));
    if (!access) return c.json({ error: "not found" }, 404);
    return withApiErrors(c, () => handle(universeClient(ctx.env, access.universe, access.caller)));
  }

  app.get("/", (c) => withConversation(c, async (client) => {
    const response = await client.call("session/read", {
      sessionId: c.req.param("sessionId"),
    });
    return c.json(response.session);
  }));

  app.get("/children", (c) => withConversation(c, async (client) => {
    const limit = Number(c.req.query("limit") ?? 50);
    const parent = c.req.param("sessionId");
    await client.authorizeOperation("session/read", { sessionId: parent });
    const response = await client.call("session/list", {
      parent,
      cursor: c.req.query("cursor") ?? null,
      limit: Number.isSafeInteger(limit) ? Math.min(Math.max(1, limit), 200) : 50,
    });
    return c.json({ sessions: response.sessions ?? [], nextCursor: response.nextCursor ?? null });
  }));

  app.post("/context/compact", (c) => withConversation(c, async (client) => {
    const response = await client.call("session/context/compact", {
      sessionId: c.req.param("sessionId"),
    });
    return c.json(response);
  }));

  /// Closing is a lifecycle transition that retains session history.
  /// `force=true` also cancels active/queued work.
  app.post("/close", (c) => withConversation(c, async (client) => {
    const body = await parseBody(c, sessionCloseSchema);
    if (!body.ok) {
      return body.response;
    }
    const response = await client.call("session/close", {
      sessionId: c.req.param("sessionId"),
      force: body.data.force ?? false,
    });
    const current = await client.call("session/read", {
      sessionId: response.session.id,
    });
    return c.json(current.session);
  }));

  /// A new execution for an open session whose last one stopped, from its log.
  app.post("/restart", (c) => withConversation(c, async (client) => {
    const response = await client.call("session/restart", {
      sessionId: c.req.param("sessionId"),
    });
    const current = await client.call("session/read", {
      sessionId: response.session.id,
    });
    return c.json(current.session);
  }));

  /// Soft deletion hides retained history and is accepted by Lightspeed only
  /// after the selected sessions are closed. Non-cascade deletion requires a
  /// leaf; cascade includes delegated children.
  app.delete("/", (c) => withConversation(c, async (client) => {
    const response = await client.call("session/delete", {
      sessionId: c.req.param("sessionId"),
      softDelete: true,
      cascade: c.req.query("cascade") === "true",
    });
    return c.json(response.session);
  }));

  /// Event-log page for the transcript. `after` is the numeric cursor seq
  /// from the previous page; `waitMs` long-polls at the engine (clamped
  /// to 30s) so the web tail follows live sessions without spinning.
  app.get("/events", (c) => withConversation(c, async (client) => {
    const direction = c.req.query("direction") ?? "forward";
    const beforeRaw = c.req.query("before");
    const before = beforeRaw === undefined ? null : Number(beforeRaw);
    const afterRaw = c.req.query("after");
    const after = afterRaw === undefined ? null : Number(afterRaw);
    const limit = Number(c.req.query("limit") ?? 200);
    const waitMs = Number(c.req.query("waitMs") ?? 0);
    if (!["forward", "backward"].includes(direction) ||
        (before !== null && (!Number.isSafeInteger(before) || before <= 0)) ||
        (after !== null && (!Number.isSafeInteger(after) || after < 0)) ||
        !Number.isSafeInteger(limit) || limit <= 0 ||
        !Number.isSafeInteger(waitMs) || waitMs < 0 ||
        (direction === "backward" && (after !== null || waitMs > 0)) ||
        (direction === "forward" && before !== null)) {
      return c.json({ error: "Invalid event pagination parameters" }, 400);
    }
    const response = await client.call("session/events/read", {
      sessionId: c.req.param("sessionId"),
      direction: direction as "forward" | "backward",
      before: before === null ? null : { seq: before },
      after: after === null ? null : { seq: after },
      limit: Math.min(limit, 500),
      waitMs: Math.min(waitMs, 30_000),
    }, { signal: c.req.raw.signal });
    return c.json(response);
  }));

  /// Shares an unshared root session with the universe, one way. The member
  /// gate lets only its creator or an admin do it.
  app.post("/share", (c) => withConversation(c, async (client) => {
    const response = await client.call("session/share", {
      sessionId: c.req.param("sessionId"),
    });
    return c.json(response);
  }));

  /// Retention belongs to the root session. Null keeps the tree until an
  /// operator deletes it; a positive duration schedules deletion after close.
  app.put("/retention", (c) => withConversation(c, async (client) => {
    const body = await parseBody(c, sessionRetentionPutSchema);
    if (!body.ok) {
      return body.response;
    }
    await client.call("session/retention/put", {
      sessionId: c.req.param("sessionId"),
      deleteAfterCloseMs: body.data.deleteAfterCloseMs,
    });
    const current = await client.call("session/read", {
      sessionId: c.req.param("sessionId"),
    });
    return c.json(current.session);
  }));

  /// Metadata is a complete map: put replaces, an empty map clears. The
  /// engine validates the bounds and rejects reserved keys.
  app.put("/metadata", (c) => withConversation(c, async (client) => {
    const body = await parseBody(c, sessionMetadataPutSchema);
    if (!body.ok) {
      return body.response;
    }
    await client.call("session/metadata/put", {
      sessionId: c.req.param("sessionId"),
      metadata: body.data.metadata,
    });
    const current = await client.call("session/read", {
      sessionId: c.req.param("sessionId"),
    });
    return c.json(current.session);
  }));

  /// Session config is a sparse whole document with optimistic concurrency.
  app.put("/config", (c) => withConversation(c, async (client) => {
    const body = await parseBody(c, sessionConfigPutSchema);
    if (!body.ok) {
      return body.response;
    }
    const response = await client.call("session/config/put", {
      sessionId: c.req.param("sessionId"),
      config: body.data.config as SessionConfig,
      expectedConfigRevision: body.data.expectedConfigRevision,
    });
    const current = await client.call("session/read", {
      sessionId: response.session.id,
    });
    return c.json(current.session);
  }));

  /// Session-local custom instructions use the engine's managed profile
  /// instruction source. This deliberately delegates reconciliation to an
  /// instruction-only inline profile apply; raw context edits would not
  /// preserve the default/profile/VFS fallback invariants.
  app.get("/instructions", (c) => withConversation(c, async (client) => {
    const response = await client.call("session/read", {
      sessionId: c.req.param("sessionId"),
    });
    const active = (response.session.activeContext.entries ?? [])
      .filter((entry) => entry.kind.type === "instructions")
      .map((entry) => ({
        key: entry.key ?? null,
        blobRef: entry.content.blobRef,
        preview: entry.preview ?? null,
      }));
    const custom = active.find((entry) => entry.key === "instructions.050.profile");
    let text: string | null = null;
    if (custom) {
      const blob = await client.call("blobs/read", { blobRef: custom.blobRef });
      text = Buffer.from(blob).toString("utf8");
    }
    return c.json({
      text,
      contextRevision: response.session.activeContext.revision,
      active,
    });
  }));

  app.put("/instructions", (c) => withConversation(c, async (client) => {
    const body = await parseBody(c, sessionInstructionsPutSchema);
    if (!body.ok) {
      return body.response;
    }
    const text = body.data.text?.trim() ? body.data.text : null;
    const response = await client.call("session/profiles/apply", {
      sessionId: c.req.param("sessionId"),
      profile: {
        kind: "inline",
        profile: text
          ? { instructions: { type: "text", text } }
          : {},
      },
    });
    return c.json(response.session);
  }));

  app.post("/environments/:environmentId/activate", (c) => withConversation(c, async (client) => {
    const response = await client.call("session/environments/activate", {
      sessionId: c.req.param("sessionId"),
      environmentId: c.req.param("environmentId"),
    });
    return c.json(response.session);
  }));

  app.post("/environments/deactivate", (c) => withConversation(c, async (client) => {
    const response = await client.call("session/environments/deactivate", {
      sessionId: c.req.param("sessionId"),
    });
    return c.json(response.session);
  }));

  /// One user message → one run from input items: its attachments as media
  /// items, then its text. Per-message model options ride the run as
  /// overrides. Returns the accepted run immediately (`running`, or `queued`
  /// behind an active run) — replies land in the event log, which the web
  /// follows via the long-poll tail. No server-side await: runs can take
  /// minutes and an HTTP request must not.
  app.post("/messages", (c) => withConversation(c, async (client) => {
    const body = await parseBody(c, sessionMessageSchema);
    if (!body.ok) {
      return body.response;
    }
    const input = body.data;
    const config = messageRunConfig(input.options);
    const response = await client.call("session/runs/start", {
      sessionId: c.req.param("sessionId"),
      source: {
        type: "input" as const,
        items: messageInputItems(input.text, input.attachments, `user:${c.get("session").user.id}`),
      },
      submissionId: input.submissionId,
      ...(config ? { config } : {}),
    });
    const run = response.run;
    return c.json({ run: { id: run.id, status: run.status } });
  }));

  app.get("/runs/:runId", (c) => withConversation(c, async (client) => {
    const response = await client.call("session/runs/read", {
      sessionId: c.req.param("sessionId"),
      runId: c.req.param("runId"),
    });
    return c.json(response.run);
  }));

  /// Cancel a queued or active run. Returns the run's projected state right
  /// after the cancel was admitted (`cancelling` for an active run,
  /// `cancelled` for a queued one); the terminal event arrives on the tail.
  app.post("/runs/:runId/cancel", (c) => withConversation(c, async (client) => {
    const response = await client.call("session/runs/cancel", {
      sessionId: c.req.param("sessionId"),
      runId: c.req.param("runId"),
    });
    const run = response.run;
    return c.json({ run: { id: run.id, status: run.status, startedAtMs: run.startedAtMs } });
  }));

  /// Steer the active run: the text and attachments are admitted into the
  /// run and reach the model at its next turn boundary without interrupting
  /// the in-flight turn. Rejected for queued, cancelling, or finished runs.
  app.post("/runs/:runId/steer", (c) => withConversation(c, async (client) => {
    const body = await parseBody(c, sessionSteerSchema);
    if (!body.ok) {
      return body.response;
    }
    const runId = c.req.param("runId");
    try {
      const response = await client.call("session/runs/steer", {
        sessionId: c.req.param("sessionId"),
        runId,
        items: messageInputItems(body.data.text, body.data.attachments, `user:${c.get("session").user.id}`),
        ...(body.data.submissionId ? { submissionId: body.data.submissionId } : {}),
      });
      const run = response.run;
      return c.json({
        steeringId: response.steeringId,
        run: { id: run.id, status: run.status },
      });
    } catch (error) {
      // The runtime has one rejection kind for both pre-submit checks and
      // admission waits. Only these pre-submit failures prove no steering
      // was sent. In particular, "run ended before steering reached it"
      // can follow submission and must retain the uncertain-outcome UI.
      if (error instanceof LightspeedApiError && error.kind === "rejected"
        && (error.message === `run is already terminal: ${runId}`
          || error.message === `run is not accepting steering: ${runId}`)) {
        return c.json({ error: error.message, kind: "steering_not_submitted" }, 409);
      }
      throw error;
    }
  }));

  app.post("/runs/:runId/approvals", (c) => withConversation(c, async (client) => {
    const body = await parseBody(c, sessionApprovalDecideSchema);
    if (!body.ok) {
      return body.response;
    }
    const response = await client.call("session/runs/approvals/decide", {
      sessionId: c.req.param("sessionId"),
      runId: c.req.param("runId"),
      decisions: body.data.decisions,
    });
    return c.json(response);
  }));

  return app;
}
