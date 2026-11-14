import { afterEach, describe, expect, it, vi } from "vitest";
import type { BotTriggerView } from "@/api";
import {
  PAIRING_ALPHABET,
  chatPayload,
  chatSpecPayload,
  defaultTriggerForms,
  inboxBotOptionIds,
  inboxSelectionSpec,
  mintPairingCode,
  sendSampleWebhook,
  sessionCloseAfterMs,
  triggerDraftSummary,
  triggerInputOf,
  triggerSecretsQuery,
} from "./triggers";

afterEach(() => vi.unstubAllGlobals());

describe("bot inbox sender selection", () => {
  it("encodes any bot with an explicit null, clearing an earlier allowlist", () => {
    expect(inboxSelectionSpec("any", ["boss"])).toEqual({ from: null });
  });

  it("encodes selected bots by immutable id", () => {
    expect(inboxSelectionSpec("selected", ["boss", "reviewer"])).toEqual({
      from: ["boss", "reviewer"],
    });
  });

  it("excludes the current bot while preserving unavailable selected ids", () => {
    expect(
      inboxBotOptionIds(
        "testoor",
        [{ botId: "testoor" }, { botId: "boss" }],
        ["retired-bot"],
      ),
    ).toEqual(["boss", "retired-bot"]);
  });
});

const chatForm = {
  channelAccountId: "acct-1",
  scope: "any" as const,
  groupActivation: "mention" as const,
  prefixesText: "/ask, /ask,\n/lightspeed",
  mentionNamesText: "",
  accessTurn: "anyone" as const,
  allowedText: "",
  controllersText: "@lukas",
  requirePairing: true,
  priority: "",
};

describe("chat trigger payload", () => {
  it("lets the server mint the pairing code on create", () => {
    const spec = chatSpecPayload(chatForm);
    expect(spec).toEqual({
      accountId: "acct-1",
      matchScope: null,
      activation: { group: "mention", triggerPrefixes: ["/ask", "/lightspeed"] },
      access: { turn: "anyone", controllers: ["@lukas"] },
      pairing: "code",
    });
    expect("pairingCode" in spec).toBe(false);
  });

  it("never names a pairing code on an edit, with pairing required or open", () => {
    const delivery = {
      routePolicy: "perKey" as const,
      routeKey: "",
      filter: "",
      whenBusy: "queue" as const,
      debounceSeconds: "",
      maxWaitSeconds: "",
      maxCount: "",
      closeMode: "forever" as const,
      closeHours: "",
    };
    expect("pairingCode" in chatPayload({ ...chatForm, ...delivery })).toBe(false);
    const open = chatPayload({ ...chatForm, ...delivery, requirePairing: false });
    expect(open.pairing).toBe("open");
    expect("pairingCode" in open).toBe(false);
  });

  it("does not send back a code it happens to hold when it replaces a trigger", () => {
    // A trigger as the list returns it has no code; even one that did must
    // not put it into the next write.
    const chat = {
      botId: "ada",
      triggerId: "telegram",
      revision: 4,
      kind: "chat",
      accountId: "acct-1",
      pairing: "code",
      pairingCode: "LeakedCode12",
      ingestPath: "/hooks/bots/u/ada/telegram/token",
      enabled: true,
      createdAtMs: 0,
      updatedAtMs: 0,
    } as unknown as BotTriggerView;
    const input = triggerInputOf(chat);
    expect(input).toMatchObject({ triggerId: "telegram", kind: "chat", accountId: "acct-1", pairing: "code" });
    expect(JSON.stringify(input)).not.toContain("LeakedCode12");
    expect(JSON.stringify(input)).not.toContain("/hooks/");
  });

  it("lists allowed handles when turns are restricted", () => {
    expect(chatSpecPayload({ ...chatForm, accessTurn: "listed", allowedText: "@a,@b" }).access).toEqual({
      turn: "listed",
      allowed: ["@a", "@b"],
      controllers: ["@lukas"],
    });
  });

  it("carries scope and priority when set", () => {
    expect(chatSpecPayload({ ...chatForm, scope: "group", priority: "5" })).toMatchObject({
      matchScope: "group",
      priority: 5,
    });
  });
});

describe("pairing codes", () => {
  it("are 12 unambiguous characters", () => {
    for (let i = 0; i < 20; i += 1) {
      const code = mintPairingCode();
      expect(code).toHaveLength(12);
      for (const char of code) expect(PAIRING_ALPHABET).toContain(char);
    }
  });
});

describe("trigger secrets", () => {
  it("are asked for one trigger at a time, again after the trigger is replaced", () => {
    const first = triggerSecretsQuery("universe", "ada", { triggerId: "github", revision: 1, createdAtMs: 10 });
    const second = triggerSecretsQuery("universe", "ada", { triggerId: "github", revision: 2, createdAtMs: 10 });
    expect(first.queryKey).toEqual(["bot-trigger-secrets", "universe", "ada", "github", 10, 1]);
    expect(second.queryKey).not.toEqual(first.queryKey);
    expect(
      triggerSecretsQuery("universe", "ada", { triggerId: "stripe", revision: 1, createdAtMs: 10 }).queryKey,
    ).not.toEqual(first.queryKey);
  });

  it("are asked for again when a trigger is deleted and created under the same name", () => {
    // The new trigger starts over at revision 1 with a new URL token or
    // code; only its creation time tells it from the one it replaced.
    const before = triggerSecretsQuery("universe", "ada", { triggerId: "github", revision: 1, createdAtMs: 10 });
    const after = triggerSecretsQuery("universe", "ada", { triggerId: "github", revision: 1, createdAtMs: 20 });
    expect(after.queryKey).not.toEqual(before.queryKey);
  });

  it("posts a sample to the ingest path it was given and to nothing without one", async () => {
    const fetched = vi.fn(async (..._request: unknown[]) => new Response(null, { status: 202 }));
    vi.stubGlobal("fetch", fetched);
    vi.stubGlobal("window", { location: { origin: "https://platform.example" } });
    await sendSampleWebhook({ preset: null }, "/hooks/bots/u/ada/github/token");
    expect(fetched).toHaveBeenCalledTimes(1);
    expect(fetched.mock.calls[0]![0]).toBe("https://platform.example/hooks/bots/u/ada/github/token");
    for (const missing of [undefined, null, ""]) {
      await expect(sendSampleWebhook({ preset: null }, missing)).rejects.toThrow("no ingest URL");
    }
    expect(fetched).toHaveBeenCalledTimes(1);
  });
});

describe("session idle close", () => {
  it("inherits with null, keeps forever with 0, and converts hours", () => {
    expect(sessionCloseAfterMs({ closeMode: "inherit", closeHours: "" })).toBeNull();
    expect(sessionCloseAfterMs({ closeMode: "forever", closeHours: "" })).toBe(0);
    expect(sessionCloseAfterMs({ closeMode: "hours", closeHours: "24" })).toBe(86_400_000);
  });
});

describe("shared trigger draft summaries", () => {
  it("describes the same draft forms used by creation and settings", () => {
    const forms = defaultTriggerForms();
    forms.poll = {
      ...forms.poll,
      sourceKind: "http",
      url: "https://example.test/issues",
      intervalMinutes: "15",
    };
    expect(triggerDraftSummary("poll", forms)).toBe("Checks https://example.test/issues every 15 min");
  });
});
