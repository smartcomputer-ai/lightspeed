/// Technical Support: Northwind's developer-support team on Lightspeed.
/// Northwind sells an events API; a helpdesk bot answers the developers
/// integrating with it over Telegram and WhatsApp from a knowledge base,
/// reproduces bug reports, and hands confirmed bugs to an engineering bot
/// that files minimal-repro issues, while a status watcher and the on-call
/// bot it pages keep the helpdesk briefed during incidents — bot federation
/// end to end, plus the profiles, KB, environment, and integrations behind it.
import type { Environment, GitHubApp, SecretGrant } from "@/api";
import { appendScriptedRun, newSession } from "../engine";
import type { DemoResponder, DemoStore, DemoToolCall, DemoTurn, UniverseState } from "../store";
import {
  BOT_TOOLS,
  DAY_MS,
  EMIT_TOOL,
  GPT,
  HOUR_MS,
  INTRODUCTION_PROMPT,
  MESSAGE_TOOLS,
  MINUTE_MS,
  NOW,
  OPUS,
  SONNET,
  ago,
  agoIso,
  at,
  atIso,
  bot,
  botEmit,
  botSession,
  botState,
  briefPut,
  channelAccount,
  channelPairing,
  chatMessage,
  chatSent,
  chatTrigger,
  eventLog,
  github,
  hex,
  inboxTrigger,
  managedSession,
  mcpCall,
  mcpServer,
  member,
  messageNoop,
  messageSend,
  modelDiscovery,
  modelOption,
  modelProvider,
  pollTrigger,
  profile,
  providerBinding,
  recent,
  runCommand,
  scheduleTrigger,
  template,
  tool,
  vfsReadFile,
  vfsWriteFile,
  webFetch,
  webhookTrigger,
  workspace,
  type Conversation,
  type ProfileInit,
  type ScriptedEvent,
  mcpDisplay,
} from "./builders";
import { TELEGRAM_ACCOUNT_ID, WHATSAPP_ACCOUNT_ID } from "./platform";

export const TECHNICAL_SUPPORT_UNIVERSE_ID = "22222222-2222-4222-8222-222222222222";
export const TECHNICAL_SUPPORT_SLUG = "technical-support";
const LIGHTSPEED_UNIVERSE_ID = "0000b0b0-0000-4000-8000-0000000b0b0b";

const PROFILE = {
  support: "support-engineer",
  triage: "incident-triage",
  escalation: "escalation-agent",
  digest: "weekly-digest",
} as const;
const WORKSPACE = { kb: "developer-kb", postmortems: "postmortems" } as const;
const ENV_SUPPORT_TOOLS = "env-support-tools";
const MCP = { zendesk: "zendesk", statuspage: "statuspage", github: "github", pagerduty: "pagerduty" } as const;
const GRANT = {
  zendesk: "grant-zendesk-oauth",
  statuspage: "grant-statuspage-token",
  anthropic: "grant-anthropic-key",
  github: "grant-github-app",
  pagerdutySigning: "grant-pagerduty-signing",
  pagerdutyApi: "grant-pagerduty-api",
} as const;
const BOT = { helpdesk: "helpdesk", escalations: "escalations", statusWatch: "status-watch", oncall: "oncall" } as const;
const STATUS_PAGE_URL = "https://status.northwind.dev";
const STATUS_FEED_URL = `${STATUS_PAGE_URL}/api/v2/incidents.json`;
const TELEGRAM_SOURCE = "telegram:northwind_support_bot";
const WHATSAPP_SOURCE = "whatsapp:+4915112345678";
const REPO = "northwind/platform";

/// Days back to the last Friday whose 15:00 digest has run (a week on Fridays).
const FRIDAY_DAYS_AGO = (new Date(NOW).getDay() + 2) % 7 || 7;

/// `YYYY-MM-DD` for search queries the digest run writes.
function dateLabel(ms: number): string {
  return new Date(ms).toISOString().slice(0, 10);
}

/// Cross-bot #N references, fixed by each log's order below: a sender quotes
/// the receiver's number in its emit result, and a receipt quotes the asked
/// event's number back. The smoke check asserts they line up.
const SEQ = {
  helpdeskNotice: 1,
  helpdeskAllClear: 4,
  escalationsBug: 4,
  statusWatchIncident: 1,
  oncallIncident: 1,
  oncallUpdate: 3,
} as const;

// ---------------------------------------------------------------------------
// Knowledge base and postmortems
// ---------------------------------------------------------------------------

const KB_AUTH = `# API authentication

Every request to \`https://api.northwind.dev/v3\` carries a credential in the \`Authorization\` header. There are two kinds.

## API keys
- Format: \`nw_live_…\` (production) or \`nw_test_…\` (sandbox), 40 characters after the prefix. Keys are shown once at creation.
- Header: \`Authorization: Bearer nw_live_…\`
- Scope: the whole account, one environment. A sandbox key on the production host (or the reverse) returns \`401 environment_mismatch\`.
- Use for server-to-server integrations. Never ship a key in a browser or mobile app.

## OAuth 2.0 (apps acting for a Northwind customer)
- Authorization code flow with PKCE; tokens from \`https://auth.northwind.dev/oauth/token\`.
- Access tokens expire after 1 hour; refresh tokens after 90 days without use.
- Header is the same: \`Authorization: Bearer <access_token>\`.
- Scopes are granted per app (\`events:read\`, \`events:write\`, \`webhooks:manage\`, \`billing:read\`). A missing scope is a \`403\`, not a \`401\`.

## 401 vs 403
| Status | \`error.code\` | Meaning | What to tell the developer |
| --- | --- | --- | --- |
| 401 | \`invalid_credentials\` | Key or token unknown, revoked, or malformed | Check the header is \`Bearer <token>\` (one space, no quotes); look for a trailing newline copied with the key |
| 401 | \`token_expired\` | OAuth access token past its hour | Refresh; do not retry the same token |
| 401 | \`environment_mismatch\` | Sandbox key on production or the reverse | Match the host to the key prefix |
| 403 | \`insufficient_scope\` | Credential is valid but lacks the scope | The response's \`required_scope\` names it; the customer re-consents |
| 403 | \`key_restricted\` | Key has an IP allowlist and the caller is outside it | Check the key's restrictions in the dashboard |

A 401 is always the credential itself; a 403 is always what it may do. Never tell a developer to "try a new key" for a 403.

## Key rotation
1. Create a second key in the dashboard (Settings → API keys → Create).
2. Deploy it; both keys work during the overlap.
3. Revoke the old key. Revocation is immediate — in-flight requests with the old key fail with \`401 invalid_credentials\`.
4. Keys unused for 180 days are flagged in the dashboard but never revoked automatically.

## Support rules
- Ask for the **request id** (\`X-Request-Id\` response header, \`req_…\`) before debugging any 401 or 403; the log line names the exact reason.
- Never ask for the key itself. The first 12 characters (\`nw_live_a1b2c3d4\`) are enough to find it.
- Plan questions ("does my plan include OAuth apps?") go to Accounts — do not guess.
`;

const KB_WEBHOOKS = `# Webhook delivery

Northwind delivers events (\`event.created\`, \`delivery.completed\`, …) to the HTTPS endpoints registered under Webhooks. Each delivery is one POST with a JSON body.

## Headers
| Header | Example | Meaning |
| --- | --- | --- |
| \`X-Northwind-Delivery-Id\` | \`dlv_01J6…\` | Unique per delivery; identical across retries of the same event |
| \`X-Northwind-Timestamp\` | \`1756377600\` | Unix seconds when the signature was computed |
| \`X-Northwind-Signature\` | \`v1=5f2a…\` | HMAC-SHA256, see below |
| \`X-Northwind-Event\` | \`event.created\` | Event type, for routing before parsing |

## Verifying the signature
The signature is HMAC-SHA256 over \`"{timestamp}.{raw_body}"\` with the endpoint's signing secret (\`whsec_…\`), hex-encoded and prefixed \`v1=\`.

1. Read the **raw request bytes**. Do not parse and re-serialize — key order, whitespace, and unicode escapes differ and the digest will not match. This is the cause of nearly every "signature never matches" ticket.
2. Reject if \`now - X-Northwind-Timestamp\` is more than **300 seconds** in either direction. This bounds replay.
3. Compute \`HMAC_SHA256(secret, timestamp + "." + raw_body)\` and compare it to the value after \`v1=\` with a constant-time comparison.
4. Only then parse the JSON.

\`\`\`js
import { createHmac, timingSafeEqual } from "node:crypto";

export function verify(rawBody, headers, secret) {
  const ts = headers["x-northwind-timestamp"];
  if (Math.abs(Date.now() / 1000 - Number(ts)) > 300) return false;
  const expected = createHmac("sha256", secret).update(\`\${ts}.\${rawBody}\`).digest("hex");
  const given = headers["x-northwind-signature"].replace(/^v1=/, "");
  return expected.length === given.length && timingSafeEqual(Buffer.from(expected), Buffer.from(given));
}
\`\`\`

Frameworks that parse JSON before your handler runs (Express \`express.json()\`, Next.js API routes, Rails) need the raw body captured explicitly — the SDKs document this per framework under "Raw body".

## Retries
A delivery counts as successful on any \`2xx\` within 10 seconds. Anything else is retried with exponential backoff and jitter:

| Attempt | Delay after the previous |
| --- | --- |
| 2 | 1 min |
| 3 | 5 min |
| 4 | 30 min |
| 5 | 2 h |
| 6 | 8 h |
| 7 (last) | 24 h |

After the last failure the endpoint is marked **failing** and the account owner gets an email; after 3 days failing, the endpoint is disabled. The dashboard shows every attempt with its status code and response body (first 1 KiB).

## Idempotency
Retries carry the same \`X-Northwind-Delivery-Id\`. Store it and skip the delivery if you have seen it — deliveries can arrive more than once even without retries during an incident (see the 2026-08-19 postmortem). Ordering is not guaranteed across events; use \`event.created_at\`.

## Support rules
- Ask for the delivery id and the endpoint URL; the dashboard's attempt log answers most questions.
- A 4xx from the developer's endpoint is their side; a \`timeout\` after 10 s usually means they do work before responding — tell them to return \`200\` first and process asynchronously.
- Signing secrets are shown once; a lost secret means rotating it in the dashboard (the old secret stays valid for 24 h alongside the new one).
`;

const KB_RATE_LIMITS = `# Rate limits

Limits apply per API key (or per OAuth app per customer) and per environment. Sandbox limits are one tenth of production.

## Limits
| Plan | Sustained | Burst | Per-endpoint notes |
| --- | --- | --- | --- |
| Developer (free) | 60 req/min | 100 in 10 s | \`/v3/events\` list: 20 req/min |
| Team | 600 req/min | 1,000 in 10 s | — |
| Business | 3,000 req/min | 5,000 in 10 s | Bulk export: 10 concurrent jobs |
| Enterprise | contractual | contractual | — |

Sustained is a sliding one-minute window; burst is a token bucket refilled at the sustained rate. Both are counted per key, so several servers sharing one key share one bucket.

## Headers on every response
- \`RateLimit-Limit: 600\`
- \`RateLimit-Remaining: 412\`
- \`RateLimit-Reset: 23\` (seconds until the window has room again)

## When the limit is hit
The response is \`429 Too Many Requests\` with \`Retry-After: <seconds>\` and \`error.code = rate_limited\`. Nothing was processed.

Clients should:
1. Sleep for \`Retry-After\` seconds (never less) and retry.
2. Back off exponentially if the retry is also a 429, and add jitter — several workers retrying on the same second refill the same bucket and collide again.
3. Use \`RateLimit-Remaining\` to slow down before hitting 429; the v3 SDKs do this with \`autoThrottle: true\`.

## Common causes
- Polling \`/v3/events\` instead of using webhooks (the list endpoint has its own lower limit for this reason).
- Many workers, one key. Create a key per service; limits are per key.
- Retrying 5xx with no backoff — during an incident this turns a 30 s blip into a 429 storm.

## Support rules
- Ask for the key prefix and the request id of one 429; the dashboard's usage chart shows the window.
- Limits are raised by Accounts for Business and Enterprise plans, not by support. Say who and how long (1 business day); do not promise a number.
- A 429 on the events list from a 60 req/min key is almost always the polling pattern — point to webhooks first.
`;

const KB_SDK = `# SDK v3 migration

v3 of the Node, Python, and Go SDKs shipped 2026-06-02. v2 receives security fixes until 2026-12-31 and no other changes; the v2 endpoints it uses stay online until 2027-06-30.

## Breaking changes
| v2 | v3 | Why |
| --- | --- | --- |
| \`client.events.on("delivery", fn)\` (long-polling listener) | \`client.events.subscribe({ types: ["delivery.completed"] })\` returns an async iterator | Long polling counted against rate limits; v3 uses a server-sent stream |
| \`client.events.list({ page: 3 })\` | \`client.events.list({ after: cursor, limit: 50 })\` | Offset pages skipped or repeated items under concurrent writes |
| \`client.webhooks.verify(body, sig)\` | \`client.webhooks.verify({ rawBody, headers })\` | The timestamp is now part of the signature |
| Errors are \`{ status, message }\` | Errors are typed: \`NorthwindError\` with \`.code\`, \`.requestId\`, \`.retryAfter\` | \`code\` is stable; \`message\` is not |
| \`apiKey\` constructor option | \`auth: { apiKey }\` or \`auth: { accessToken, refresh }\` | OAuth apps |
| Sync methods (Python) | All methods async; \`northwind.sync\` wraps them for scripts | — |

Everything else — resource names, field names, pagination cursors — is unchanged.

## Codemod
\`npx @northwind/codemod v2-to-v3 ./src\` (Node) and \`northwind-codemod v2-to-v3 ./app\` (Python) rewrite the table above where the call shape is static. They cannot rewrite:
- Event listeners with side effects inside the callback (you get a \`TODO(northwind-v3)\` comment and a \`subscribe\` skeleton).
- Offset pagination loops — the codemod flags them; rewrite as \`for await (const event of client.events.list({ limit: 50 }))\`.

## Timeline
- 2026-06-02 — v3 released; v2 deprecated (warning at startup).
- 2026-12-31 — last v2 release.
- 2027-06-30 — v2 long-polling endpoint removed; v2 clients fail with \`410 Gone\`.

## Support rules
- Ask which SDK and version (\`npm ls @northwind/sdk\`, \`pip show northwind\`) and which call broke.
- The \`events.on\` removal is the most common ticket: send the \`subscribe\` example and the codemod command. Open a ticket tagged \`sdk-v3\` so we count them.
- Requests to extend the v2 deadline go to Accounts, not support.
`;

const KB_SANDBOX = `# Sandbox vs production

| | Sandbox | Production |
| --- | --- | --- |
| Host | \`https://api.sandbox.northwind.dev/v3\` | \`https://api.northwind.dev/v3\` |
| Keys | \`nw_test_…\` | \`nw_live_…\` |
| Data | Reset every Sunday 02:00 UTC; seeded with 500 sample events | Persistent |
| Rate limits | 1/10 of the plan's production limits | Plan limits |
| Webhooks | Delivered, but retries stop after attempt 3 | Full retry schedule |
| Billing | Free, no metering | Metered |
| Status page component | "Sandbox API" | "API", "Webhooks", "Dashboard" |

## Things that differ on purpose
- Sandbox event ids are prefixed \`evt_test_\`; code that validates ids by prefix breaks on the switch.
- Sandbox webhook deliveries are signed with the sandbox endpoint's own \`whsec_test_…\` secret.
- The sandbox runs the **next** API build one week before production. A bug that appears only in sandbox may be a real regression — escalate it with the build id from the \`X-Northwind-Build\` response header.

## Moving to production checklist
1. Swap host and key together (a sandbox key on production is \`401 environment_mismatch\`).
2. Register production webhook endpoints separately; sandbox endpoints are not copied.
3. Re-check rate-limit assumptions — production limits are 10× higher, but so is traffic.
4. Remove any \`evt_test_\` handling.

## Support rules
- Ask which environment before anything else; half of "the API is down" reports are the Sunday sandbox reset.
- Sandbox data loss is expected and not a ticket. A sandbox reset that did not happen, or happened at the wrong time, is.
`;

const KB_ESCALATION = `# Escalation matrix

Support owns the conversation; engineering owns the fix. An escalation hands over a reproducible fact, not a feeling.

## When a report is a bug
All three, or it stays with support:
1. **Reproduced** by support in the sandbox or with the developer's request ids — not just reported.
2. **Documented behavior differs** — the KB or the reference says X, the API does Y. Undocumented behavior is a feature request.
3. **Minimal repro** — one request (or one short sequence) that shows it, with request ids, key type, and the exact response.

## Severity
| Severity | Definition | Example | Route | Response |
| --- | --- | --- | --- | --- |
| 1 | Production API or webhooks failing for many customers, or wrong data delivered | Deliveries dropped; another customer's events returned | \`bot_emit\` to \`oncall\` (incident) **and** \`escalations\` (issue) | 15 min |
| 2 | A documented feature broken for everyone using it; a workaround exists | Cursor pagination returns duplicates; an SDK method throws | \`bot_emit\` to \`escalations\`, kind \`bug.confirmed\` | 1 business day to triage |
| 3 | Wrong for some inputs, edge cases, sandbox-only, docs wrong | Trailing-slash redirect URI rejected; typo in a field description | \`bot_emit\` to \`escalations\`, kind \`bug.confirmed\`, \`severity: 3\` | 1 week to triage |

## What the escalation carries
- Summary in one sentence: endpoint, input, expected, actual.
- Request ids (at least two, showing it is not one bad node).
- Key type (sandbox or production), SDK and version if relevant, \`X-Northwind-Build\`.
- The Zendesk ticket id, so the developer hears back.

## What is not an escalation
| Situation | Goes to | How |
| --- | --- | --- |
| Rate-limit increase, plan or billing question | Accounts | Zendesk group **Accounts**, 1 business day |
| Feature request | Product | Zendesk tag \`feature-request\`; tell the developer it is logged, no date |
| Security report | Security | Zendesk group **Security**, tag \`security\`; do not discuss details in chat |
| Outage in progress | On-call | \`bot_emit\` to \`oncall\`; the status page is the developer-facing answer |
| Abusive or threatening developer | Team lead (Jonas) | Assign directly; stop replying |

## Rules
- Escalate once, with everything above. An issue without a repro comes back.
- Tell the developer the issue number when engineering files it, and the fix version when it ships — the ticket stays open until then.
- Do not promise a date. "Engineering has it as northwind/platform#N; I will update this thread when it ships."
`;

const KB_IDEMPOTENCY = `# Idempotency keys

Retries are how clients survive timeouts and 5xx; idempotency is how retries stay safe. Northwind supports it in both directions.

## Outbound: \`Idempotency-Key\` on POST requests
- Send \`Idempotency-Key: <unique string>\` (up to 128 characters; a UUID is the usual choice) on any \`POST\` to \`/v3/events\`, \`/v3/webhooks\`, or \`/v3/exports\`.
- The first request with a key is processed normally. A retry with the same key **and the same body** within 24 hours returns the stored response — same status, same body, plus \`Idempotent-Replayed: true\`.
- Same key, different body: \`409 idempotency_key_reused\`. Nothing is processed.
- Keys are scoped to the API key (or OAuth app + customer) and the environment; two integrations cannot collide.
- A request that failed with 5xx stores nothing — retry it with the same key.
- The v3 SDKs set a key on every POST and reuse it across their own retries (\`retries: 3\` by default); pass \`idempotencyKey\` on the call to control it.

\`\`\`js
await client.events.create(
  { type: "order.shipped", data: { orderId: "o_812" } },
  { idempotencyKey: \`order-shipped-\${order.id}\` },
);
\`\`\`
Derive the key from the business action (\`order-shipped-o_812\`), not from the attempt — that is what makes a retry a retry.

## Inbound: deduplicating deliveries
Webhook deliveries carry \`X-Northwind-Delivery-Id\`, identical across retries of the same event and stable during incidents. Store it (a unique index is enough) and skip the delivery if it is already there. Details in /kb/webhook-delivery.md under Idempotency.

## Common mistakes
- A new key per attempt (\`Date.now()\`): every retry creates a new event.
- A key per customer, not per action: the second order for a customer is "replayed" as the first.
- Reusing a key after changing the body: \`409\` — generate a new key for a new action.

## Support rules
- A "duplicate events" report starts with: which side (they created twice, or we delivered twice), and the request ids or delivery ids.
- Two events with different \`Idempotent-Replayed\` values are two requests with two keys; ask how the key is derived.
- The replay window and key length are fixed; requests to change them are feature requests.
`;

const KB_FILES: Record<string, string> = {
  "api-authentication.md": KB_AUTH,
  "webhook-delivery.md": KB_WEBHOOKS,
  "rate-limits.md": KB_RATE_LIMITS,
  "sdk-v3-migration.md": KB_SDK,
  "sandbox-vs-production.md": KB_SANDBOX,
  "escalation-matrix.md": KB_ESCALATION,
  "idempotency-keys.md": KB_IDEMPOTENCY,
};

const POSTMORTEM_DELAYS = `# Postmortem: webhook delivery delays, 2026-08-19

**Severity:** 2 · **Duration:** 14:02–15:20 CEST (78 min) · **Owner:** Jonas Lindqvist · **Status:** action items open

## Impact
Webhook deliveries for EU-region accounts were delayed by 40 s to 4 min 10 s; 1.9 M deliveries were affected and none were lost. 212 deliveries went out twice with the same \`X-Northwind-Delivery-Id\` once the backlog drained. 27 developers wrote in; the helpdesk bot answered 26 from the incident notice without a ticket.

## Timeline (CEST)
- 13:55 — Deploy \`delivery-worker\` 2026.08.19-2 to eu-1: the new build serializes the delivery record before signing, so each attempt costs about 3× the CPU.
- 14:02 — Queue-depth alert; the deploy owner opens the status-page incident. \`status-watch\` pages \`oncall\`, which acknowledges PD-4482 and opens #inc-0819.
- 14:08 — \`oncall\` briefs \`helpdesk\`: deliveries are queued, not dropped.
- 14:31 — First developer report in the Northwind Developers group; answered from the notice.
- 14:40 — Rollback to 2026.08.19-1 starts; the worker fleet drains at ~14 k deliveries/s.
- 15:03 — Drain complete for 4 of 5 shards; shard 3 re-sends 212 deliveries whose ack was lost in the rollback.
- 15:20 — Backlog gone; incident to monitoring. Resolved 15:45.

## Root cause
The delivery worker's new build signed a re-serialized copy of the delivery record instead of the stored bytes. That tripled CPU per delivery and the eu-1 fleet fell behind its intake. The re-serialization also changed the body bytes, so the few developers who verify signatures over the raw bytes saw mismatches until the rollback (they were right; we were not).

## What went well
- No delivery was lost: the queue is durable and the backlog drained in 40 minutes.
- Detection to page: 3 minutes, no human in the loop. Helpdesk had the developer wording within 6 minutes.
- Retries carried the original delivery id, so developers who deduplicate saw nothing.

## What went badly
- The canary did not catch it: it measures error rate, not CPU per delivery.
- 212 duplicate deliveries: the rollback did not wait for in-flight acks on shard 3.
- The status-page text said "delayed" but not "not dropped"; the notice to helpdesk had to add that.

## Action items
| # | Action | Owner | Due | Status |
| --- | --- | --- | --- | --- |
| 1 | Sign the stored bytes; test that the signed body is byte-identical to the delivered body | Sofia Brandt | 2026-08-26 | done |
| 2 | Canary on CPU-seconds per delivery, not only errors | Jonas Lindqvist | 2026-09-02 | open |
| 3 | Rollback drains in-flight acks before cutting over | Sofia Brandt | 2026-09-09 | open |
| 4 | Status-page template for delivery delays says "queued, not dropped" and gives the dedupe advice | Priya Natarajan | 2026-08-29 | open |
| 5 | KB article on idempotency (delivery ids inbound, \`Idempotency-Key\` on POSTs outbound) | Priya Natarajan | 2026-09-05 | open |
`;

const POSTMORTEM_FILES: Record<string, string> = {
  "2026-08-19-webhook-delivery-delays.md": POSTMORTEM_DELAYS,
};

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

const SUPPORT_INSTRUCTIONS = `You are Northwind's developer-support engineer. You answer the developers integrating with the Northwind API on Telegram and WhatsApp on behalf of the support team.

Tone: precise, friendly, no filler. Short paragraphs; code in code blocks; one question at a time. Assume the developer is competent and busy.

Answer from the knowledge base first: read the relevant file under /kb (authentication, webhook delivery, rate limits, SDK v3 migration, sandbox vs production, escalation matrix) before answering anything it covers, and quote its numbers — windows, limits, timelines — rather than remembering them. If the KB does not cover it, say what you can verify and open a Zendesk ticket instead of guessing.

Always ask for request ids (the X-Request-Id header, req_…) before debugging an error. The first 12 characters of a key are enough to find it — never ask for a whole key or a secret.

Never guess about billing, plan limits, or contract terms: those go to Accounts, and you say so.

Bugs: a report becomes an escalation only when you have reproduced it (the nw CLI in your environment has a sandbox key and a read-only production key) and have a minimal repro — endpoint, input, expected, actual, two request ids, key type, build. Then bot_emit to escalations with kind bug.confirmed and the ticket id, tell the developer engineering has it, and relay the issue number when the receipt arrives. Follow /kb/escalation-matrix.md for severity and for what is not an escalation.

Tickets: open a Zendesk ticket for anything that needs a person or a follow-up, and link it in your reply. In groups, reply only when mentioned or replied to, and keep it to what was asked.`;

const TRIAGE_INSTRUCTIONS = `You are Northwind's incident triage agent. You watch the status page and PagerDuty, decide what deserves attention, and keep the people on call informed.

Severity: 1 = the production API or webhook delivery failing for many customers, or wrong data delivered; 2 = a documented feature degraded for everyone using it (delivery delays, elevated 5xx, auth failures); 3 = sandbox-only, one region, or a non-core service. Scheduled maintenance is not an incident.

For every new incident: fetch the status-page detail, classify the severity, and act by severity — page for 1 and 2, note 3 in the log. Acknowledge PagerDuty pages with the pagerduty tools; open #inc-<mmdd> for severity 1 and 2 with the slack CLI in your environment. An update to an incident you already handled needs action only when the severity or the developer-facing impact changes.

Brief the helpdesk bot when developers will notice: one line on what is broken, what to tell them (queued or dropped, retry or wait, dedupe advice), and when to expect an update. Send the all-clear too.

Be terse. Every message you send is read by someone who is already busy.`;

const ESCALATION_INSTRUCTIONS = `You turn confirmed bugs from support into engineering issues in northwind/platform, and you are the only path from support to the issue tracker.

For every bug.confirmed event: read /kb/escalation-matrix.md if you have not since it last changed, then search open and recently closed issues for the same behavior — endpoint, error code, symptom — before filing anything. A duplicate gets no new issue: say which issue it is and stop; the receipt tells support.

A new issue is minimal: title = endpoint + symptom; body = expected, actual, one curl or SDK snippet that reproduces it, request ids, key type, build id, the Zendesk ticket, and the severity from the matrix as a label (sev-1, sev-2, sev-3). No speculation about the cause. Add the area label from the path (api-events, webhooks, auth, sdk) and from-support.

Fridays you post the weekly digest as a comment on the tracking issue: bugs filed this week, bugs closed, and anything sev-2 older than five business days without an assignee.`;

const DIGEST_INSTRUCTIONS = `You write Northwind's weekly developer-support digest for the Monday support review.

Sources, in this order: Zendesk metrics for the week, /postmortems for incidents, /kb/escalation-matrix.md for what counts as an escalation. Never invent numbers; if a metric is missing, say so and move on.

Format: one page. Headline numbers first (conversations, tickets, median first response, escalations to engineering, issues filed), then the top issues as a table with counts and the KB page that answers each, then three bullets on what changed and why, then open action items with owners. Plain language, no adjectives. Flag any topic with more than five tickets that has no KB page.`;

const KB_LINK = { path: "/kb", access: "readOnly", target: { type: "workspace", workspaceId: WORKSPACE.kb } };

const SUPPORT_PROFILE: ProfileInit = {
  profileId: PROFILE.support,
  displayName: "Support engineer",
  description: "Answers developers from the KB and Zendesk; reproduces reports and escalates confirmed bugs with a minimal repro.",
  instructions: SUPPORT_INSTRUCTIONS,
  config: {
    model: SONNET,
    generation: { reasoningEffort: "low" },
    features: {
      vfs: { tools: "readOnly", workspaceLinks: [KB_LINK] },
      environments: {},
      mcp: { servers: [{ serverId: MCP.zendesk }] },
    },
  },
  environment: { type: "existing", environmentId: ENV_SUPPORT_TOOLS },
  revision: 9,
  createdAtMs: ago(44 * DAY_MS),
  updatedAtMs: at(1, 16, 20),
};

const TRIAGE_PROFILE: ProfileInit = {
  profileId: PROFILE.triage,
  displayName: "Incident triage",
  description: "Classifies status-page and PagerDuty incidents and acts by severity from the support-tools environment.",
  instructions: TRIAGE_INSTRUCTIONS,
  config: {
    model: GPT,
    generation: { reasoningEffort: "medium" },
    features: {
      environments: {},
      web: { fetch: {} },
      mcp: { servers: [{ serverId: MCP.statuspage }, { serverId: MCP.pagerduty }] },
    },
    limits: { maxToolRounds: 12 },
  },
  environment: { type: "existing", environmentId: ENV_SUPPORT_TOOLS },
  revision: 4,
  createdAtMs: ago(31 * DAY_MS),
  updatedAtMs: ago(3 * DAY_MS),
};

const ESCALATION_PROFILE: ProfileInit = {
  profileId: PROFILE.escalation,
  displayName: "Escalation agent",
  description: "Files minimal-repro issues in northwind/platform for bugs support has confirmed; never a duplicate.",
  instructions: ESCALATION_INSTRUCTIONS,
  config: {
    model: OPUS,
    generation: { reasoningEffort: "high" },
    features: {
      vfs: { tools: "readOnly", workspaceLinks: [KB_LINK] },
      mcp: {
        servers: [{ serverId: MCP.github }],
      },
    },
    limits: { maxToolRounds: 8 },
  },
  revision: 4,
  createdAtMs: ago(21 * DAY_MS),
  updatedAtMs: ago(5 * DAY_MS),
};

const DIGEST_PROFILE: ProfileInit = {
  profileId: PROFILE.digest,
  displayName: "Weekly digest",
  description: "One-page Monday digest from Zendesk metrics, postmortems, and the KB.",
  instructions: DIGEST_INSTRUCTIONS,
  config: {
    model: OPUS,
    generation: { reasoningEffort: "high", maxOutputTokens: 16_000 },
    features: {
      vfs: {
        tools: "readOnly",
        workspaceLinks: [
          KB_LINK,
          { path: "/postmortems", access: "readOnly", target: { type: "workspace", workspaceId: WORKSPACE.postmortems } },
        ],
      },
      mcp: { servers: [{ serverId: MCP.zendesk }] },
    },
    limits: { maxTurns: 40 },
  },
  revision: 2,
  createdAtMs: ago(20 * DAY_MS),
  updatedAtMs: ago(6 * DAY_MS),
};

/// The support profile with the KB link writable, for a session that
/// drafts an article in place.
const KB_AUTHOR_CONFIG: Record<string, unknown> = {
  model: SONNET,
  generation: { reasoningEffort: "medium" },
  features: {
    vfs: { tools: "readWrite", workspaceLinks: [{ ...KB_LINK, access: "readWrite" }] },
  },
};

// ---------------------------------------------------------------------------
// Conversations and the tool calls the builders do not cover
// ---------------------------------------------------------------------------

const DANA: Conversation = {
  sessionId: `bot:v1:${BOT.helpdesk}:k-telegram-dm-dana-okafor`,
  label: "telegram dm · Dana Okafor",
  provider: "telegram",
  source: TELEGRAM_SOURCE,
  chatId: "5521",
  scope: "direct",
};
const DEVS_GROUP: Conversation = {
  sessionId: `bot:v1:${BOT.helpdesk}:k-telegram-group-northwind-developers`,
  label: "telegram group · Northwind Developers",
  provider: "telegram",
  source: TELEGRAM_SOURCE,
  chatId: "-1002391",
  scope: "group",
};
const WA_DEV: Conversation = {
  sessionId: `bot:v1:${BOT.helpdesk}:k-whatsapp-dm-447700900123`,
  label: "whatsapp dm · +44 7700 900123",
  provider: "whatsapp",
  source: WHATSAPP_SOURCE,
  chatId: "447700900123@s.whatsapp.net",
  scope: "direct",
};
const LEE: Conversation = {
  sessionId: `bot:v1:${BOT.helpdesk}:k-telegram-dm-lee-zhang`,
  label: "telegram dm · Lee Zhang",
  provider: "telegram",
  source: TELEGRAM_SOURCE,
  chatId: "7734",
  scope: "direct",
};

const main = (botId: string): { sessionId: string; label: string } => ({ sessionId: `bot:v1:${botId}`, label: "Main" });

/// A PagerDuty MCP call that the server refused.
function pagerdutyFailure(name: string, args: Record<string, unknown>, output: string): DemoToolCall {
  return tool(`pagerduty.${name}`, `pagerduty.${name}`, args, mcpDisplay(`pagerduty.${name}`, args), output, true);
}

/// Support's `nw` CLI in the support-tools environment.
function nw(argv: string[], output: string): DemoToolCall {
  return runCommand(["nw", ...argv], output, { detail: ENV_SUPPORT_TOOLS, cwd: "/home/support", timeoutMs: 30_000 });
}

// ---------------------------------------------------------------------------
// Universe resources
// ---------------------------------------------------------------------------

/// The universe's channel accounts and the pairing rows binding each
/// seeded conversation to the helpdesk's chat triggers.
function seedChannels(universe: UniverseState): void {
  channelAccount(universe, {
    accountId: TELEGRAM_ACCOUNT_ID,
    provider: "telegram",
    providerAccountId: "northwind_support_bot",
    displayName: "Northwind Developer Support (Telegram)",
    credentialGrantId: "grant-telegram-bot-token",
    createdAtMs: Date.parse("2026-07-02T10:00:00.000Z"),
    updatedAtMs: Date.parse("2026-08-15T10:00:00.000Z"),
  });
  channelAccount(universe, {
    accountId: WHATSAPP_ACCOUNT_ID,
    provider: "whatsapp",
    providerAccountId: "+4915112345678",
    displayName: "Northwind Developer Support (WhatsApp)",
    settings: { printQr: false },
    createdAtMs: Date.parse("2026-07-20T10:00:00.000Z"),
    updatedAtMs: Date.parse("2026-08-22T10:00:00.000Z"),
  });
  for (const conversation of [DEVS_GROUP, DANA, LEE]) {
    channelPairing(universe, {
      accountId: TELEGRAM_ACCOUNT_ID,
      botId: BOT.helpdesk,
      triggerId: "telegram",
      chatId: conversation.chatId,
      pairedVia: "open",
      pairedAtMs: ago(12 * DAY_MS),
    });
  }
  channelPairing(universe, {
    accountId: WHATSAPP_ACCOUNT_ID,
    botId: BOT.helpdesk,
    triggerId: "whatsapp",
    chatId: WA_DEV.chatId,
    pairedVia: "code",
    pairedAtMs: ago(9 * DAY_MS),
  });
}

function seedMembers(store: DemoStore, universe: UniverseState): void {
  universe.members.push(
    member(store, universe, "user-jonas", "admin", ago(47 * DAY_MS)),
    member(store, universe, "user-priya", "member", ago(32 * DAY_MS)),
  );
  universe.apiKeys.push({
    keyPrefix: "lsk_ts_4c9e",
    displayName: "Developer dashboard support widget",
    createdAtMs: ago(26 * DAY_MS),
    lastUsedAtMs: ago(9 * MINUTE_MS),
    revokedAtMs: null,
  });
}

function seedProfiles(universe: UniverseState): void {
  for (const init of [SUPPORT_PROFILE, TRIAGE_PROFILE, ESCALATION_PROFILE, DIGEST_PROFILE]) {
    universe.profiles.set(init.profileId, profile(init));
  }
}

function seedWorkspaces(store: DemoStore, universe: UniverseState): void {
  workspace(store, universe, {
    id: WORKSPACE.kb,
    displayName: "Developer knowledge base",
    files: KB_FILES,
    revision: 32,
    createdAtMs: ago(46 * DAY_MS),
    updatedAtMs: ago(HOUR_MS + 36 * MINUTE_MS),
  });
  workspace(store, universe, {
    id: WORKSPACE.postmortems,
    displayName: "Postmortems",
    files: POSTMORTEM_FILES,
    revision: 5,
    createdAtMs: ago(18 * DAY_MS),
    updatedAtMs: ago(7 * DAY_MS),
  });
}

function seedEnvironments(universe: UniverseState): void {
  const requestId = `req-${hex("support-tools", 12)}`;
  const environment: Environment = {
    environmentId: ENV_SUPPORT_TOOLS,
    requestId,
    source: { type: "provisioned", providerId: "incus-eu-1", bindingId: "incus-eu-1" },
    displayName: "Support tools",
    status: "paused",
    desiredPower: "paused",
    idlePolicy: { pauseAfterMs: 30 * MINUTE_MS, stopAfterMs: 12 * HOUR_MS },
    incarnation: {
      incarnationId: `inc-${hex("support-tools-inc", 10)}`,
      provisionRequestId: requestId,
      providerTargetId: "ls-support-tools-7b2e91",
      templateId: "dev-small-v1",
      powerStates: ["running", "paused", "stopped"],
      createdAtMs: ago(29 * DAY_MS),
      updatedAtMs: ago(2 * HOUR_MS + 6 * MINUTE_MS),
    },
    publicIngressEnabled: false,
    publicEndpoint: null,
    metadata: { role: "support-tooling", tools: "nw (sandbox + read-only production key), slack, jq, curl" },
    createdAtMs: ago(29 * DAY_MS),
    updatedAtMs: ago(2 * HOUR_MS + 6 * MINUTE_MS),
  };
  universe.environments.set(ENV_SUPPORT_TOOLS, environment);
  universe.providerBindings.push(
    providerBinding({ revision: 2, metadata: { project: "technical-support" }, createdAtMs: ago(30 * DAY_MS), updatedAtMs: ago(30 * DAY_MS) }),
  );
  universe.environmentTemplates.push(
    template({
      templateId: "dev-small-v1",
      displayName: "Development VM (small)",
      description: "2 vCPU / 4 GiB, Git, Docker, common toolchains, envd.",
      publicIngress: true,
      deprecated: false,
      metadata: { cpu: "2", memory: "4GiB", disk: "40GiB" },
    }),
    template({
      templateId: "dev-large-v1",
      displayName: "Development VM (large)",
      description: "8 vCPU / 16 GiB, same image as small.",
      publicIngress: true,
      deprecated: false,
      metadata: { cpu: "8", memory: "16GiB", disk: "120GiB" },
    }),
    template({
      templateId: "dev-small-v0",
      displayName: "Development VM (small, previous)",
      description: "Superseded image; kept for existing environments.",
      publicIngress: false,
      deprecated: true,
      metadata: {},
    }),
  );
  universe.environmentCredentials.push({
    environmentId: ENV_SUPPORT_TOOLS,
    envName: "STATUSPAGE_TOKEN",
    source: { type: "authGrant", grantId: GRANT.statuspage },
    createdAtMs: ago(28 * DAY_MS),
    updatedAtMs: ago(28 * DAY_MS),
  });
}

function seedIntegrations(universe: UniverseState): void {
  for (const server of [
    mcpServer({
      serverId: MCP.zendesk,
      displayName: "Zendesk",
      serverUrl: "https://northwind-dev.zendesk.com/mcp",
      description: "Tickets, requesters, and weekly metrics for the developer helpdesk.",
      allowedTools: ["search_tickets", "get_ticket", "create_ticket", "add_ticket_comment", "close_ticket", "get_ticket_metrics"],
      approvalDefault: "never",
      deferLoadingDefault: false,
      authPolicy: { type: "requiredOAuth", resource: "https://northwind-dev.zendesk.com/mcp" },
      credential: { type: "authGrant", grantId: GRANT.zendesk },
      status: "active",
      revision: 5,
      createdAtMs: ago(40 * DAY_MS),
      updatedAtMs: ago(5 * DAY_MS),
    }),
    mcpServer({
      serverId: MCP.statuspage,
      displayName: "Statuspage",
      serverUrl: `${STATUS_PAGE_URL}/mcp`,
      description: "Incident and component state from the public status page.",
      deferLoadingDefault: true,
      authPolicy: { type: "requiredBearer" },
      credential: { type: "authGrant", grantId: GRANT.statuspage },
      status: "unverified",
      createdAtMs: ago(2 * DAY_MS),
      updatedAtMs: ago(2 * DAY_MS),
    }),
    mcpServer({
      serverId: MCP.github,
      displayName: "GitHub",
      serverUrl: "https://api.githubcopilot.com/mcp/",
      description: "GitHub's hosted MCP server, scoped to the northwind organisation through the App installation.",
      allowedTools: ["search_issues", "get_issue", "create_issue", "add_issue_comment", "list_issues"],
      approvalDefault: "never",
      authPolicy: { type: "gitHubApp", providerId: "github" },
      credential: { type: "authGrant", grantId: GRANT.github },
      status: "active",
      revision: 3,
      createdAtMs: ago(21 * DAY_MS),
      updatedAtMs: ago(12 * DAY_MS),
    }),
    mcpServer({
      serverId: MCP.pagerduty,
      displayName: "PagerDuty",
      serverUrl: "https://mcp.pagerduty.com/mcp",
      description: "Incidents on the Northwind API service: acknowledge, list, trigger.",
      allowedTools: ["list_incidents", "get_incident", "acknowledge", "trigger"],
      approvalDefault: "never",
      authPolicy: { type: "requiredBearer" },
      credential: { type: "authGrant", grantId: GRANT.pagerdutyApi },
      status: "active",
      revision: 2,
      createdAtMs: ago(29 * DAY_MS),
      updatedAtMs: ago(29 * DAY_MS),
    }),
  ]) {
    universe.mcpServers.set(server.serverId, server);
  }

  const githubApp: GitHubApp = {
    providerId: "github",
    providerKind: "gitHubApp",
    displayName: "northwind-platform",
    config: { type: "githubApp", appId: "1204577", apiBaseUrl: "https://api.github.com" },
    hasCredential: true,
    status: "active",
    createdAtMs: ago(21 * DAY_MS),
    updatedAtMs: ago(21 * DAY_MS),
  };
  universe.githubApps.push(githubApp);

  const grants: SecretGrant[] = [
    {
      grantId: GRANT.zendesk,
      providerId: MCP.zendesk,
      providerKind: "mcpOAuth",
      displayName: "Zendesk (devsupport@northwind.dev)",
      subjectHint: "devsupport@northwind.dev",
      status: "active",
      exposure: "brokered",
      principal: { kind: "universeDefault" },
      scopes: ["tickets:read", "tickets:write", "users:read"],
      audience: null,
      hasAccessToken: true,
      hasRefreshToken: true,
      expiresAtMs: NOW + 23 * MINUTE_MS,
      lastLeasedAtMs: ago(2 * HOUR_MS + 41 * MINUTE_MS),
      leaseCount: 402,
      createdAtMs: ago(40 * DAY_MS),
      updatedAtMs: ago(37 * MINUTE_MS),
    },
    {
      grantId: GRANT.statuspage,
      providerId: MCP.statuspage,
      providerKind: "staticBearer",
      displayName: "Statuspage API token",
      subjectHint: "northwind",
      status: "active",
      exposure: "retrievable",
      principal: { kind: "serviceAccount", id: BOT.statusWatch },
      scopes: ["incidents:read"],
      audience: STATUS_PAGE_URL,
      hasAccessToken: true,
      hasRefreshToken: false,
      expiresAtMs: null,
      lastLeasedAtMs: ago(80_000),
      leaseCount: 21_604,
      createdAtMs: ago(30 * DAY_MS),
      updatedAtMs: ago(30 * DAY_MS),
    },
    {
      grantId: GRANT.anthropic,
      providerId: "anthropic",
      providerKind: "modelApiKey",
      displayName: "Anthropic API key",
      subjectHint: "sk-ant-…c4Q2",
      status: "active",
      exposure: "brokered",
      principal: { kind: "universeDefault" },
      hasAccessToken: true,
      hasRefreshToken: false,
      expiresAtMs: null,
      lastLeasedAtMs: ago(3 * MINUTE_MS),
      leaseCount: 11_207,
      createdAtMs: ago(47 * DAY_MS),
      updatedAtMs: ago(47 * DAY_MS),
    },
    {
      grantId: GRANT.github,
      providerId: "github",
      providerKind: "gitHubApp",
      displayName: "northwind-platform · northwind",
      subjectHint: "northwind",
      status: "active",
      exposure: "brokered",
      principal: { kind: "universeDefault" },
      scopes: [],
      hasAccessToken: true,
      hasRefreshToken: false,
      expiresAtMs: NOW + 41 * MINUTE_MS,
      lastLeasedAtMs: ago(2 * HOUR_MS + 40 * MINUTE_MS),
      leaseCount: 58,
      metadata: {
        installation_id: 51730284,
        account_login: "northwind",
        repository_selection: "selected",
        permissions: { issues: "write", metadata: "read", contents: "read" },
      },
      createdAtMs: ago(21 * DAY_MS),
      updatedAtMs: ago(2 * HOUR_MS + 40 * MINUTE_MS),
    },
    {
      grantId: GRANT.pagerdutySigning,
      providerId: "pagerduty",
      providerKind: "staticBearer",
      displayName: "PagerDuty webhook signing secret",
      subjectHint: "northwind-api",
      status: "active",
      exposure: "brokered",
      principal: { kind: "serviceAccount", id: BOT.oncall },
      hasAccessToken: true,
      hasRefreshToken: false,
      expiresAtMs: null,
      lastLeasedAtMs: ago(2 * HOUR_MS + 50 * MINUTE_MS),
      leaseCount: 63,
      createdAtMs: ago(29 * DAY_MS),
      updatedAtMs: ago(29 * DAY_MS),
    },
    {
      grantId: GRANT.pagerdutyApi,
      providerId: "pagerduty",
      providerKind: "staticBearer",
      displayName: "PagerDuty API token",
      subjectHint: "oncall-bot@northwind.dev",
      status: "needsReauth",
      exposure: "brokered",
      principal: { kind: "serviceAccount", id: BOT.oncall },
      audience: "https://api.pagerduty.com",
      hasAccessToken: true,
      hasRefreshToken: false,
      expiresAtMs: null,
      lastLeasedAtMs: ago(4 * DAY_MS),
      leaseCount: 149,
      metadata: { note: "Rejected by PagerDuty since 2026-08-24 (token rotated there); re-enter the API token." },
      createdAtMs: ago(29 * DAY_MS),
      updatedAtMs: ago(4 * DAY_MS),
    },
  ];
  universe.secrets = {
    providers: [
      modelProvider("anthropic", "Anthropic", { type: "modelApiKey", endpoint: null }, true, ago(47 * DAY_MS), ago(47 * DAY_MS)),
      modelProvider("openai", "OpenAI", { type: "modelApiKey", endpoint: null }, true, ago(31 * DAY_MS), ago(31 * DAY_MS)),
    ],
    grants,
  };

  const fetchedAtMs = ago(6 * MINUTE_MS);
  universe.models = {
    models: [
      modelOption(SONNET, "Claude Sonnet 5", { maxInputTokens: 200_000, maxOutputTokens: 64_000, parallelToolUse: true, reasoningEfforts: ["none", "low", "medium", "high"] }, fetchedAtMs),
      modelOption(OPUS, "Claude Opus 5", { maxInputTokens: 200_000, maxOutputTokens: 128_000, parallelToolUse: true, reasoningEfforts: ["none", "low", "medium", "high", "xhigh"] }, fetchedAtMs),
      modelOption(GPT, "GPT-5.4", { maxInputTokens: 272_000, maxOutputTokens: 128_000, parallelToolUse: true, reasoningEfforts: ["none", "low", "medium", "high", "xhigh"] }, fetchedAtMs),
      modelOption({ ...GPT, model: "gpt-5.4-mini" }, "GPT-5.4 mini", { maxInputTokens: 272_000, maxOutputTokens: 128_000, parallelToolUse: true, reasoningEfforts: ["none", "low", "medium", "high"] }, fetchedAtMs),
    ],
    providers: [
      modelDiscovery("anthropic", ["anthropic:messages"], "configured", "universe", fetchedAtMs),
      modelDiscovery("openai", ["openai:responses", "openai:completions"], "configured", "universe", fetchedAtMs),
    ],
  };

  universe.setups.push({
    id: "configurator",
    name: "Configurator",
    description:
      "Creates a dedicated credential, registers the Configurator MCP server, and adds a ready-to-use profile for managing this universe.",
    version: 3,
    available: true,
    status: "available",
  });
}

// ---------------------------------------------------------------------------
// Developer Helpdesk: developers over Telegram and WhatsApp
// ---------------------------------------------------------------------------

const HELPDESK_BRIEF = `Northwind developer helpdesk. Ask for request ids (req_…) before debugging; the first 12 characters of a key are enough. Current SDKs: v3 (Node 3.1.2, Python 3.1.0, Go 3.0.4); v2 gets security fixes only until 2026-12-31. Sandbox is on build 2026.08.27-3, production on 2026.08.20-1.

No incident notice in effect.`;

const HELPDESK_BRIEF_INCIDENT = `Northwind developer helpdesk. Ask for request ids (req_…) before debugging; the first 12 characters of a key are enough. Current SDKs: v3 (Node 3.1.2, Python 3.1.0, Go 3.0.4); v2 gets security fixes only until 2026-12-31.

Current notice (until 15:00 today, from oncall): EU webhook deliveries are delayed up to 4 minutes (#inc-0819) — queued, not dropped; retries carry the original X-Northwind-Delivery-Id, so anyone deduplicating by it needs no action.`;

const DELAY_NOTICE =
  "Webhook deliveries for EU accounts are delayed up to 4 minutes (sev 2, #inc-0819). Tell developers: deliveries are queued, not dropped; retries carry the original X-Northwind-Delivery-Id, so deduplicating by it needs no action. Update by 15:00.";
const ALL_CLEAR_NOTICE =
  "#inc-0819 resolved: the EU webhook backlog drained at 15:20. 212 deliveries went out twice with the same delivery id between 15:03 and 15:20; anyone not deduplicating by X-Northwind-Delivery-Id may see doubles from that window. No other action.";

const BUG_CURSOR_SUMMARY =
  "Confirmed bug (sev 2, api-events): GET /v3/events?after=<cursor>&limit=50 returns the cursor's own event as the first item of the next page, at every boundary. Sandbox build 2026.08.27-3; production 2026.08.20-1 pages correctly. Request ids req_01J6F3M8Q2 (page 1, last item evt_test_8f3a2c) and req_01J6F3N1KD (page 2 with after=evt_test_8f3a2c, first item evt_test_8f3a2c); reproduced as req_01J6F4C2VD / req_01J6F4C4RQ. Plain curl. ZD-5107.";
const BUG_CURSOR_DATA = {
  severity: 2,
  area: "api-events",
  endpoint: "GET /v3/events",
  build: "2026.08.27-3",
  requestIds: ["req_01J6F3M8Q2", "req_01J6F3N1KD", "req_01J6F4C2VD", "req_01J6F4C4RQ"],
  ticket: "ZD-5107",
};
const ISSUE_2287 = `${REPO}#2287`;

const SAM_DELAY_REPLY =
  "Not lost — there is an incident open right now (#inc-0819, status.northwind.dev): webhook deliveries for EU accounts are delayed by up to 4 minutes while a backlog drains. Every delivery is queued and will arrive, and retries carry the original X-Northwind-Delivery-Id, so if you deduplicate by it you need to do nothing. Next update by 15:00; I will post here when it clears.";
const SAM_429_REPLY =
  "Nothing is open on the status page, so this is the limit itself. On the Team plan the events list shares the key's 600 req/min sustained window and the 1,000-in-10-s burst bucket — and every server using the same key shares one bucket. Two things to check: (1) the 429 response carries Retry-After; sleep at least that long and add jitter, otherwise several workers retry on the same second and collide again; (2) if you are polling /v3/events for new events, switch to webhooks — the list endpoint is the one that trips first. If you send me the request id of one 429 (X-Request-Id, req_…) I can see which window it hit.";
const DANA_REPLY = `Two things cause that, and Express makes the first one easy to hit:

1. The signature is over "{timestamp}.{raw_body}" — the value of X-Northwind-Timestamp, a dot, then the request bytes exactly as sent. If express.json() runs before your handler, req.body is already parsed and re-serializing it changes the bytes. Capture the raw body: express.json({ verify: (req, _res, buf) => { req.rawBody = buf.toString("utf8"); } })
2. Include the timestamp and check it: createHmac("sha256", secret).update(\`\${ts}.\${rawBody}\`).digest("hex"), compare it to the header value after v1= with timingSafeEqual, and reject when the timestamp is more than 300 s from now.

The full example is in the docs under Webhooks → Verifying deliveries. If it still fails, send me one delivery id (X-Northwind-Delivery-Id) and I will check the attempt log.`;
const WA_SDK_REPLY = `Not by hand. In v3 the long-polling listener became a stream: for await (const event of client.events.subscribe({ types: ["delivery.completed"] })) { await handler(event); } — same event objects, so your handler body stays. Run the codemod first: npx @northwind/codemod v2-to-v3 ./src rewrites the static call shapes and leaves a TODO(northwind-v3) comment plus a subscribe skeleton wherever a listener had side effects inside the callback. The other v3 changes that bite most people: offset pages (page: n) became cursors (after), and errors are typed now (err.code, err.requestId). v2 keeps getting security fixes until 2026-12-31. I have opened ZD-5102 so we can follow up if the codemod leaves you anything odd: https://northwind-dev.zendesk.com/agent/tickets/5102`;
const LEE_ASK =
  "That would be a bug — cursors are exclusive by the reference. Before I hand it to engineering I need enough to reproduce it: (1) the request ids (X-Request-Id, req_…) of two consecutive pages that overlap, (2) the cursor you passed and the event id that appears twice, (3) the SDK and version if you are not using curl. The two curl commands themselves would be ideal.";
const LEE_CONFIRMED =
  "Reproduced it: on the sandbox (build 2026.08.27-3) the after cursor is inclusive at every boundary — page 2 starts with the event you passed as the cursor. Production is on 2026.08.20-1 and pages correctly, so this is a regression in next week's build. I have escalated it to engineering with your two request ids and the curl repro; your ticket is ZD-5107. Workaround until it ships: skip the first item when its id equals the cursor. I will post the issue number here once it is filed.";
const LEE_ISSUE = `Engineering has it as ${ISSUE_2287} (severity 2, api-events). The build is still sandbox-only, so production is unaffected, and the fix is expected before that build promotes. I will update this thread when it ships — ZD-5107 stays open until then.`;

function seedHelpdesk(store: DemoStore, universe: UniverseState): void {
  const record = bot(universe, {
    botId: BOT.helpdesk,
    displayName: "Developer Helpdesk",
    description: "Answers developers over Telegram and WhatsApp from the KB and Zendesk; escalates confirmed bugs.",
    profileId: PROFILE.support,
    brief: HELPDESK_BRIEF,
    runsPerDay: 500,
    breaker: { fires: 30, windowMs: 10 * MINUTE_MS },
    emit: true,
    createdAtMs: ago(44 * DAY_MS),
    updatedAtMs: ago(2 * HOUR_MS + 37 * MINUTE_MS),
  });

  const triggers = new Map([
    [
      "telegram",
      chatTrigger(
        BOT.helpdesk,
        "telegram",
        {
          accountId: TELEGRAM_ACCOUNT_ID,
          matchScope: null,
          activation: { group: "mention", mentionNames: ["@northwind_support_bot"] },
          access: { turn: "anyone" },
          pairingCode: null,
          priority: 10,
        },
        { createdAtMs: ago(44 * DAY_MS), updatedAtMs: ago(12 * DAY_MS) },
      ),
    ],
    [
      "whatsapp",
      chatTrigger(
        BOT.helpdesk,
        "whatsapp",
        {
          accountId: WHATSAPP_ACCOUNT_ID,
          matchScope: "direct",
          activation: null,
          access: { turn: "anyone" },
          pairingCode: "NW-DEV-7K2Q",
          priority: 20,
        },
        { createdAtMs: ago(30 * DAY_MS), deliver: { whenBusy: "queue" } },
      ),
    ],
    [
      "inbox",
      inboxTrigger(BOT.helpdesk, [BOT.oncall, BOT.escalations], {
        route: { policy: "bot" },
        deliver: { whenBusy: "queue" },
        createdAtMs: ago(29 * DAY_MS),
        updatedAtMs: ago(21 * DAY_MS),
      }),
    ],
  ]);

  // The numbered log, oldest first. Group messages reach the bot only when
  // it is addressed; sends are archived so the model can refer to them by #N.
  const log = eventLog(store, BOT.helpdesk);
  const notice = (kind: string, summary: string, atMs: number, detail: string, data: unknown): ScriptedEvent =>
    log.add({
      kind,
      source: `bot:${BOT.oncall}`,
      at: atMs,
      summary,
      eventId: `bot:${BOT.oncall}:${hex(`${kind}:${summary}`, 24)}`,
      session: main(BOT.helpdesk),
      sender: BOT.oncall,
      hops: 1,
      outcome: "handled",
      detail,
      data,
    });
  const e1 = notice("incident.notice", DELAY_NOTICE, at(9, 14, 8), "Brief updated with the delivery-delay notice; developers asking about missing webhooks get the queued-not-dropped wording", { incident: "#inc-0819", severity: 2, until: "15:00" });
  const e2 = chatMessage(log, DEVS_GROUP, "telegram", "Sam R.", "@northwind_support_bot our webhook deliveries stopped arriving about 20 minutes ago, nothing changed on our side. EU account. Are they lost?", at(9, 14, 31), "handled", "Answered from the incident notice: queued, not dropped; retries carry the original delivery id");
  const e3 = chatSent(log, DEVS_GROUP, SAM_DELAY_REPLY, at(9, 14, 32), e2.envelope.seq ?? 0);
  const e4 = notice("incident.notice", ALL_CLEAR_NOTICE, at(9, 15, 23), "Brief reset: notice dropped; the 15:03–15:20 duplicate window kept for anyone asking today", { incident: "#inc-0819", severity: 2, resolved: true });
  const e5 = chatMessage(log, DEVS_GROUP, "telegram", "Sam R.", "@northwind_support_bot getting 429s on GET /v3/events since this morning, maybe 1 in 5 requests. Team plan. Is something up?", at(5, 9, 48), "handled", "Rate-limit answer from /kb/rate-limits.md: Team window, honour Retry-After with jitter, webhooks instead of polling");
  const e6 = chatSent(log, DEVS_GROUP, SAM_429_REPLY, at(5, 9, 49), e5.envelope.seq ?? 0);
  const e7 = chatMessage(log, DEVS_GROUP, "telegram", "Nadia F.", "@Sam we hit the same thing last month, the SDK's autoThrottle option fixed it for us", at(5, 10, 3), "ignored", "Reply in the bot's thread but addressed to Sam, not the bot; stayed quiet");
  const e8 = chatMessage(log, DANA, "telegram", "Dana Okafor", "Hi — my webhook signature check fails on every delivery. I'm computing HMAC-SHA256 over the JSON body with the signing secret from the dashboard but it never matches X-Northwind-Signature. Node 22, Express.", at(3, 14, 12), "handled", "Walked through raw-body hashing and the 300 s timestamp tolerance from /kb/webhook-delivery.md");
  const e9 = chatSent(log, DANA, DANA_REPLY, at(3, 14, 13), e8.envelope.seq ?? 0);
  log.add({
    kind: "chat.message",
    source: WHATSAPP_SOURCE,
    at: at(2, 11, 6),
    summary: "+44 7700 900456 (11:06): hello is this northwind api support? my integration broke after the sdk update",
    eventId: "chat:whatsapp:447700900456@s.whatsapp.net:1",
    session: null,
    outcome: "blocked",
    detail: "Pairing required: +44 7700 900456 is not paired with this bot; the pairing hint was sent",
    resolvedAfterMs: 1_000,
    data: { sender: { name: "+44 7700 900456" }, text: "hello is this northwind api support? my integration broke after the sdk update", isDirect: true },
  });
  const e11 = chatMessage(log, WA_DEV, "whatsapp", "+44 7700 900123", "Upgrading to SDK v3 (node) and client.events.on('delivery', handler) is gone — TypeError: client.events.on is not a function. Do I have to rewrite all my listeners by hand?", at(2, 16, 40), "handled", "SDK v3 migration: events.on became subscribe(); codemod sent; ticket ZD-5102 opened");
  const e12 = chatSent(log, WA_DEV, WA_SDK_REPLY, at(2, 16, 42), e11.envelope.seq ?? 0);
  const e13 = chatMessage(log, LEE, "telegram", "Lee Zhang", "I think I found a bug. GET /v3/events?after=<cursor>&limit=50 returns the last item of the previous page again as the first item of the next page — every page boundary, not just once. Sandbox key.", ago(3 * HOUR_MS + 5 * MINUTE_MS), "handled", "Asked for two consecutive request ids, the cursor, and the client before escalating");
  const e14 = chatSent(log, LEE, LEE_ASK, ago(3 * HOUR_MS + 4 * MINUTE_MS), e13.envelope.seq ?? 0);
  const e15 = chatMessage(log, LEE, "telegram", "Lee Zhang", "Sure:\ncurl -H 'Authorization: Bearer nw_test_9f2c…' 'https://api.sandbox.northwind.dev/v3/events?limit=50' → req_01J6F3M8Q2, last item evt_test_8f3a2c\ncurl -H 'Authorization: Bearer nw_test_9f2c…' 'https://api.sandbox.northwind.dev/v3/events?limit=50&after=evt_test_8f3a2c' → req_01J6F3N1KD, first item evt_test_8f3a2c again\nPlain curl, no SDK. X-Northwind-Build: 2026.08.27-3.", ago(2 * HOUR_MS + 42 * MINUTE_MS), "handled", `Reproduced on sandbox build 2026.08.27-3: cursor inclusive at every boundary; ticket ZD-5107; escalated to escalations (#${SEQ.escalationsBug} there) with the repro`);
  const e16 = chatSent(log, LEE, LEE_CONFIRMED, ago(2 * HOUR_MS + 41 * MINUTE_MS), e15.envelope.seq ?? 0);
  const e17 = log.add({
    kind: "bot.reply",
    source: `bot:${BOT.escalations}`,
    at: ago(2 * HOUR_MS + 38 * MINUTE_MS),
    summary: `#${SEQ.escalationsBug} at ${BOT.escalations} finished handled: Filed ${ISSUE_2287} (sev-2, api-events): /v3/events cursor inclusive at page boundaries on build 2026.08.27-3; no duplicate found`,
    eventId: `reply:${BOT.escalations}:${hex(`dlv-${BOT.escalations}-${SEQ.escalationsBug}\nbug.confirmed:${BOT.helpdesk}:cursor`, 24)}`,
    session: LEE,
    sender: BOT.escalations,
    hops: 2,
    inReplyTo: { bot: BOT.escalations, seq: SEQ.escalationsBug },
    outcome: "handled",
    detail: `Relayed ${ISSUE_2287} to Lee; ZD-5107 stays open until the fix ships`,
    data: { status: "handled" },
  });
  const e18 = chatSent(log, LEE, LEE_ISSUE, ago(2 * HOUR_MS + 37 * MINUTE_MS), e17.envelope.seq ?? 0);

  // Sessions: the main desk plus one thread per conversation.
  const tools = [...BOT_TOOLS, EMIT_TOOL, ...MESSAGE_TOOLS];
  const desk = managedSession(store, universe, {
    id: main(BOT.helpdesk).sessionId,
    botId: BOT.helpdesk,
    displayName: "Developer Helpdesk",
    profile: SUPPORT_PROFILE,
    tools: [...BOT_TOOLS, EMIT_TOOL],
    createdAtMs: ago(44 * DAY_MS),
    environmentId: ENV_SUPPORT_TOOLS,
  });
  appendScriptedRun(store, desk, {
    at: ago(44 * DAY_MS) + 20_000,
    user: INTRODUCTION_PROMPT,
    steps: [
      {
        text: "I am the Developer Helpdesk, Northwind's developer-support engineer: I answer developers on Telegram (mentions in groups, every direct message) and on paired WhatsApp numbers, one thread per conversation. I read the knowledge base under /kb, work tickets through Zendesk, reproduce reports with the nw CLI in the support-tools environment, and take notices from on-call through my inbox.\n\nTwo things to check: the WhatsApp connection is still waiting for its phone, and my brief is empty — tell me the current SDK versions and any notice in effect and I will keep them there.",
      },
    ],
  });
  e1.envelope.runId = appendScriptedRun(store, desk, {
    at: at(9, 14, 8),
    user: e1.prompt,
    steps: [
      {
        thinking: "A notice from on-call about webhook delivery. Developers will ask whether deliveries are lost; the wording belongs in my brief so every conversation thread sees it, with the 15:00 expiry.",
        tools: [briefPut(HELPDESK_BRIEF_INCIDENT)],
        text: "Added the delivery-delay notice to my brief with the 15:00 expiry. Developers asking about missing webhooks get: queued, not dropped, retries carry the original delivery id. No tickets for this one.",
      },
    ],
  }).id;
  e4.envelope.runId = appendScriptedRun(store, desk, {
    at: at(9, 15, 23),
    user: e4.prompt,
    steps: [
      {
        tools: [briefPut(`${HELPDESK_BRIEF}\n\nToday only: #inc-0819 (EU webhook delays) resolved 15:20; 212 deliveries went out twice between 15:03 and 15:20 with the same delivery id — anyone not deduplicating by X-Northwind-Delivery-Id may have seen doubles.`)],
        text: "Brief reset: the notice is out, and the duplicate window stays in for the rest of today for anyone asking about doubles.",
      },
    ],
  }).id;

  const group = managedSession(store, universe, {
    id: DEVS_GROUP.sessionId,
    botId: BOT.helpdesk,
    displayName: DEVS_GROUP.label,
    profile: SUPPORT_PROFILE,
    tools,
    createdAtMs: at(9, 14, 31) - 5_000,
    environmentId: ENV_SUPPORT_TOOLS,
  });
  e2.envelope.runId = appendScriptedRun(store, group, {
    at: at(9, 14, 31),
    user: e2.prompt,
    steps: [
      {
        tools: [messageSend(DEVS_GROUP, SAM_DELAY_REPLY, e2.envelope.seq ?? 0, e3.envelope.seq ?? 0)],
        text: `Answered from the incident notice in my brief (#${e3.envelope.seq}): queued, not dropped, dedupe by delivery id, update by 15:00. No ticket.`,
      },
    ],
  }).id;
  e5.envelope.runId = appendScriptedRun(store, group, {
    at: at(5, 9, 48),
    user: e5.prompt,
    steps: [
      {
        thinking: "No notice in my brief, so nothing is open — this is the rate limit itself. One in five since the morning sounds like polling the list endpoint. The KB has the Team numbers; quote them.",
        tools: [vfsReadFile("/kb/rate-limits.md", KB_RATE_LIMITS)],
      },
      {
        tools: [messageSend(DEVS_GROUP, SAM_429_REPLY, e5.envelope.seq ?? 0, e6.envelope.seq ?? 0)],
        text: `Quoted the Team window and burst bucket from /kb/rate-limits.md, Retry-After with jitter, and webhooks instead of polling (#${e6.envelope.seq}). Asked for one 429's request id.`,
      },
    ],
  }).id;
  e7.envelope.runId = appendScriptedRun(store, group, {
    at: at(5, 10, 3),
    user: e7.prompt,
    steps: [
      {
        tools: [messageNoop(DEVS_GROUP, "addressed to Sam, not the bot")],
        text: "Nadia is answering Sam in my thread; nothing asked of me. Staying quiet.",
      },
    ],
  }).id;

  const dana = managedSession(store, universe, {
    id: DANA.sessionId,
    botId: BOT.helpdesk,
    displayName: DANA.label,
    profile: SUPPORT_PROFILE,
    tools,
    createdAtMs: at(3, 14, 12) - 5_000,
    environmentId: ENV_SUPPORT_TOOLS,
  });
  e8.envelope.runId = appendScriptedRun(store, dana, {
    at: at(3, 14, 12),
    user: e8.prompt,
    steps: [
      {
        thinking: "Signature never matches, on Express — nearly always the parsed-and-re-serialized body. The KB has the exact recipe: raw bytes, timestamp in the signed string, 300 s tolerance. Answer with the Express raw-body capture.",
        tools: [vfsReadFile("/kb/webhook-delivery.md", KB_WEBHOOKS)],
      },
      {
        tools: [messageSend(DANA, DANA_REPLY, e8.envelope.seq ?? 0, e9.envelope.seq ?? 0)],
        text: `Raw-body capture for Express plus the timestamp step from /kb/webhook-delivery.md (#${e9.envelope.seq}). Asked for a delivery id if it still fails; no ticket yet.`,
      },
    ],
  }).id;

  const wa = managedSession(store, universe, {
    id: WA_DEV.sessionId,
    botId: BOT.helpdesk,
    displayName: WA_DEV.label,
    profile: SUPPORT_PROFILE,
    tools,
    createdAtMs: at(2, 16, 40) - 5_000,
    environmentId: ENV_SUPPORT_TOOLS,
  });
  e11.envelope.runId = appendScriptedRun(store, wa, {
    at: at(2, 16, 40),
    user: e11.prompt,
    steps: [
      {
        thinking: "The events.on removal — the most common v3 ticket. The KB has the subscribe replacement and the codemod; open a ticket tagged sdk-v3 so it is counted.",
        tools: [vfsReadFile("/kb/sdk-v3-migration.md", KB_SDK)],
      },
      {
        tools: [
          mcpCall("zendesk.create_ticket", { subject: "SDK v3: client.events.on removed — migration help", requester: "+44 7700 900123 (WhatsApp)", tags: ["sdk-v3", "whatsapp"], body: "Node SDK v3 upgrade; long-polling listener replaced by events.subscribe(). Sent the subscribe example and the codemod command." }, { id: 5102, url: "https://northwind-dev.zendesk.com/agent/tickets/5102" }),
        ],
      },
      {
        tools: [messageSend(WA_DEV, WA_SDK_REPLY, e11.envelope.seq ?? 0, e12.envelope.seq ?? 0)],
        text: `Sent the subscribe() replacement and the codemod command, with the other two v3 changes that usually bite (#${e12.envelope.seq}). ZD-5102 opened and tagged sdk-v3.`,
      },
    ],
  }).id;

  const lee = managedSession(store, universe, {
    id: LEE.sessionId,
    botId: BOT.helpdesk,
    displayName: LEE.label,
    profile: SUPPORT_PROFILE,
    tools,
    createdAtMs: ago(3 * HOUR_MS + 5 * MINUTE_MS) - 5_000,
    environmentId: ENV_SUPPORT_TOOLS,
  });
  e13.envelope.runId = appendScriptedRun(store, lee, {
    at: ago(3 * HOUR_MS + 5 * MINUTE_MS),
    user: e13.prompt,
    steps: [
      {
        thinking: "A cursor page repeating the boundary item would be a real bug — cursors are exclusive by the reference. The matrix wants it reproduced with request ids before anything goes to engineering. Ask for exactly what I need, once.",
        tools: [vfsReadFile("/kb/escalation-matrix.md", KB_ESCALATION)],
      },
      {
        tools: [messageSend(LEE, LEE_ASK, e13.envelope.seq ?? 0, e14.envelope.seq ?? 0)],
        text: `Asked Lee for two consecutive request ids, the cursor and the duplicated id, and the client (#${e14.envelope.seq}). Nothing to escalate until I can reproduce it.`,
      },
    ],
  }).id;
  e15.envelope.runId = appendScriptedRun(store, lee, {
    at: ago(2 * HOUR_MS + 42 * MINUTE_MS),
    user: e15.prompt,
    steps: [
      {
        thinking: "Two request ids, the cursor, plain curl, build 2026.08.27-3. Reproduce it with the sandbox key in the support-tools environment, and check production is not affected, before escalating.",
        tools: [
          nw(["events", "list", "--env", "sandbox", "--limit", "50", "--json"], '{"request_id":"req_01J6F4C2VD","build":"2026.08.27-3","count":50,"first":"evt_test_91b0aa","last":"evt_test_8f3a2c","next_cursor":"evt_test_8f3a2c"}'),
          nw(["events", "list", "--env", "sandbox", "--limit", "50", "--after", "evt_test_8f3a2c", "--json"], '{"request_id":"req_01J6F4C4RQ","build":"2026.08.27-3","count":50,"first":"evt_test_8f3a2c","last":"evt_test_7c11e0","next_cursor":"evt_test_7c11e0"}'),
          nw(["events", "list", "--env", "production", "--limit", "2", "--after", "evt_5d0c41", "--json"], '{"request_id":"req_01J6F4C6MX","build":"2026.08.20-1","count":2,"first":"evt_5d0c8a","last":"evt_5d0c9f","next_cursor":"evt_5d0c9f"}'),
        ],
      },
      {
        tools: [
          mcpCall("zendesk.create_ticket", { subject: "Confirmed bug: /v3/events cursor inclusive at page boundaries (sandbox 2026.08.27-3)", requester: "Lee Zhang (Telegram)", tags: ["bug-confirmed", "api-events", "telegram"], body: BUG_CURSOR_SUMMARY }, { id: 5107, url: "https://northwind-dev.zendesk.com/agent/tickets/5107" }),
          botEmit({ to: BOT.escalations, kind: "bug.confirmed", summary: BUG_CURSOR_SUMMARY, data: BUG_CURSOR_DATA, reply: true }, SEQ.escalationsBug),
        ],
      },
      {
        tools: [messageSend(LEE, LEE_CONFIRMED, e15.envelope.seq ?? 0, e16.envelope.seq ?? 0)],
        text: `Reproduced: sandbox pages are inclusive at the cursor (req_01J6F4C2VD → req_01J6F4C4RQ), production pages correctly. Severity 2 by the matrix. Opened ZD-5107, escalated to escalations (their #${SEQ.escalationsBug}) with the repro and a receipt requested, and told Lee with the skip-the-first-item workaround (#${e16.envelope.seq}).`,
      },
    ],
  }).id;
  e17.envelope.runId = appendScriptedRun(store, lee, {
    at: ago(2 * HOUR_MS + 38 * MINUTE_MS),
    user: e17.prompt,
    steps: [
      {
        tools: [messageSend(LEE, LEE_ISSUE, e15.envelope.seq ?? 0, e18.envelope.seq ?? 0)],
        text: `Engineering filed ${ISSUE_2287}; relayed it to Lee (#${e18.envelope.seq}). ZD-5107 stays open until the fix ships.`,
      },
    ],
  }).id;

  const state = botState({
    bot: record,
    sessions: [botSession(desk, "main"), botSession(lee, "keyed"), botSession(wa, "keyed"), botSession(dana, "keyed"), botSession(group, "keyed")],
    recentEvents: [
      recent(e17.envelope, { inputTokens: 15_240, cachedInputTokens: 14_610 }),
      recent(e15.envelope, { inputTokens: 14_050, cachedInputTokens: 12_710 }),
      recent(e13.envelope, { inputTokens: 11_980, cachedInputTokens: 11_020 }),
      recent(e11.envelope, { inputTokens: 12_330, cachedInputTokens: 11_480 }),
      recent(e8.envelope, { inputTokens: 12_140, cachedInputTokens: 11_300 }),
    ],
    eventsProcessed: 1_412,
    appliedProfileRevision: SUPPORT_PROFILE.revision,
    runsToday: 3,
  });
  universe.bots.set(BOT.helpdesk, { bot: record, triggers, events: log.events, state, descendants: [] });
}

// ---------------------------------------------------------------------------
// Engineering Escalations: confirmed bugs become issues
// ---------------------------------------------------------------------------

const ESCALATIONS_BRIEF = `Tracking issue for the weekly digest: ${REPO}#2200. Labels: sev-1/2/3, from-support, and the area (api-events, webhooks, auth, sdk, dashboard). Sofia Brandt triages api-events and webhooks; Priya Natarajan triages auth and sdk. A sev-2 without an assignee after five business days is named in the digest with @northwind/platform-leads.`;

const ISSUE_2287_BODY = `**Endpoint:** GET /v3/events?after=<cursor>&limit=50
**Expected:** the page after \`after=evt_test_8f3a2c\` starts with the next event.
**Actual:** it starts with \`evt_test_8f3a2c\` itself; every boundary repeats the cursor's event.
**Environment:** sandbox, build 2026.08.27-3. Production (2026.08.20-1) pages correctly.

**Repro**
\`\`\`
curl -H 'Authorization: Bearer nw_test_…' 'https://api.sandbox.northwind.dev/v3/events?limit=50'
# req_01J6F3M8Q2 — last item evt_test_8f3a2c, next_cursor evt_test_8f3a2c
curl -H 'Authorization: Bearer nw_test_…' 'https://api.sandbox.northwind.dev/v3/events?limit=50&after=evt_test_8f3a2c'
# req_01J6F3N1KD — first item evt_test_8f3a2c
\`\`\`
Reproduced by support: req_01J6F4C2VD / req_01J6F4C4RQ.

**Support ticket:** ZD-5107`;

function seedEscalations(store: DemoStore, universe: UniverseState): void {
  const record = bot(universe, {
    botId: BOT.escalations,
    displayName: "Engineering Escalations",
    description: "Turns support's confirmed bugs into minimal-repro issues in northwind/platform; posts the Friday bug digest.",
    profileId: PROFILE.escalation,
    brief: ESCALATIONS_BRIEF,
    runsPerDay: 60,
    breaker: { fires: 10, windowMs: 10 * MINUTE_MS },
    emit: true,
    createdAtMs: ago(21 * DAY_MS),
    updatedAtMs: ago(5 * DAY_MS),
  });
  const triggers = new Map([
    [
      "inbox",
      inboxTrigger(BOT.escalations, [BOT.helpdesk], { route: { policy: "bot" }, deliver: { whenBusy: "queue" }, createdAtMs: ago(21 * DAY_MS) }),
    ],
    [
      "weekly-bug-digest",
      scheduleTrigger(
        BOT.escalations,
        "weekly-bug-digest",
        {
          cron: "0 15 * * 5",
          summary: `Post the weekly bug digest as a comment on ${REPO}#2200: issues filed from support this week, issues closed, and any sev-2 older than five business days without an assignee.`,
        },
        { route: { policy: "bot" }, deliver: { whenBusy: "queue" }, createdAtMs: ago(20 * DAY_MS) },
      ),
    ],
  ]);

  const log = eventLog(store, BOT.escalations);
  const confirmed = (summary: string, atMs: number, outcome: "handled" | "ignored", detail: string, data: unknown): ScriptedEvent =>
    log.add({
      kind: "bug.confirmed",
      source: `bot:${BOT.helpdesk}`,
      at: atMs,
      summary,
      eventId: `bot:${BOT.helpdesk}:${hex(`bug.confirmed:${summary}`, 24)}`,
      session: main(BOT.escalations),
      sender: BOT.helpdesk,
      hops: 1,
      outcome,
      detail,
      resolvedAfterMs: 80_000,
      data,
    });
  const x1 = confirmed(
    "Confirmed bug (sev 3, auth): POST /oauth/token returns 500 internal_error when redirect_uri ends with a trailing slash that the registered URI also has; without the slash on both, 200. Request ids req_01J5ZQ7R2M, req_01J5ZQ9X0C. Sandbox and production. ZD-5074.",
    at(11, 16, 5),
    "handled",
    `Filed ${REPO}#2263 (sev-3, auth): OAuth token exchange 500 on trailing-slash redirect_uri`,
    { severity: 3, area: "auth", endpoint: "POST /oauth/token", requestIds: ["req_01J5ZQ7R2M", "req_01J5ZQ9X0C"], ticket: "ZD-5074" },
  );
  const x2 = confirmed(
    "Confirmed bug (sev 2, webhooks): retry attempt 6 fires 2 h after attempt 5 instead of the documented 8 h. Delivery dlv_01J60V3K9Q, attempts at 09:12 and 11:12. Production key. ZD-5081.",
    at(10, 11, 20),
    "ignored",
    `Duplicate of open ${REPO}#2241 (attempt 6 uses the attempt-5 delay); no new issue`,
    { severity: 2, area: "webhooks", deliveryId: "dlv_01J60V3K9Q", ticket: "ZD-5081" },
  );
  const digestAt = at(FRIDAY_DAYS_AGO, 15, 0);
  const weekStart = dateLabel(digestAt - 7 * DAY_MS);
  const x3 = log.add({
    kind: "schedule",
    source: "schedule:weekly-bug-digest",
    at: digestAt,
    summary: `Post the weekly bug digest as a comment on ${REPO}#2200: issues filed from support this week, issues closed, and any sev-2 older than five business days without an assignee.`,
    eventId: `schedule:weekly-bug-digest:${atIso(digestAt)}`,
    session: main(BOT.escalations),
    outcome: "handled",
    detail: "Digest posted on #2200: 3 filed, 2 closed, #2241 sev-2 unassigned for 6 business days — leads mentioned",
    data: { trigger: "weekly-bug-digest", cron: "0 15 * * 5", timezone: "Europe/Berlin", scheduledAt: atIso(digestAt) },
  });
  const x4 = confirmed(
    BUG_CURSOR_SUMMARY,
    ago(2 * HOUR_MS + 41 * MINUTE_MS),
    "handled",
    `No duplicate; filed ${ISSUE_2287} (sev-2, api-events) with the curl repro; receipt to ${BOT.helpdesk}`,
    BUG_CURSOR_DATA,
  );

  const session = managedSession(store, universe, {
    id: main(BOT.escalations).sessionId,
    botId: BOT.escalations,
    displayName: "Engineering Escalations",
    profile: ESCALATION_PROFILE,
    tools: [...BOT_TOOLS, EMIT_TOOL],
    createdAtMs: ago(21 * DAY_MS),
  });
  appendScriptedRun(store, session, {
    at: ago(21 * DAY_MS) + 20_000,
    user: INTRODUCTION_PROMPT,
    steps: [
      {
        text: `I am Engineering Escalations: I take bug.confirmed events from helpdesk through my inbox, check ${REPO} for duplicates, and file minimal-repro issues through the GitHub App — and on Fridays at 15:00 I post the digest on the tracking issue. I read the escalation matrix under /kb.\n\nOne thing to confirm: which issue is the tracking issue for the digest, and which labels engineering wants on filed issues.`,
      },
    ],
  });
  x1.envelope.runId = appendScriptedRun(store, session, {
    at: at(11, 16, 5),
    user: x1.prompt,
    steps: [
      {
        thinking: "Trailing-slash redirect URI on the token endpoint, two request ids, both environments — a sev-3 by the matrix. Check for an existing issue before filing.",
        tools: [
          vfsReadFile("/kb/escalation-matrix.md", KB_ESCALATION),
          github("search_issues", { q: `repo:${REPO} is:issue oauth redirect_uri trailing slash` }, "duplicates?", "0 results"),
        ],
      },
      {
        tools: [
          github(
            "create_issue",
            { owner: "northwind", repo: "platform", title: "POST /oauth/token returns 500 when redirect_uri has a trailing slash", labels: ["sev-3", "auth", "from-support"], body: "**Expected:** 200 when redirect_uri matches the registered URI byte for byte (both with a trailing slash).\n**Actual:** 500 internal_error; without the slash on both sides, 200.\n**Request ids:** req_01J5ZQ7R2M (sandbox), req_01J5ZQ9X0C (production).\n**Repro:** register `https://example.test/cb/`, then POST /oauth/token with the same redirect_uri.\n**Support ticket:** ZD-5074" },
            "sev-3 · auth",
            `created ${REPO}#2263 https://github.com/northwind/platform/issues/2263`,
          ),
        ],
        text: `Filed ${REPO}#2263 (sev-3, auth, from-support): expected 200, actual 500 on a trailing-slash redirect_uri, two request ids, ZD-5074. No duplicate. Helpdesk gets the receipt when this run finishes.`,
      },
    ],
  }).id;
  x2.envelope.runId = appendScriptedRun(store, session, {
    at: at(10, 11, 20),
    user: x2.prompt,
    steps: [
      {
        tools: [
          github("search_issues", { q: `repo:${REPO} is:issue is:open label:webhooks retry` }, "open webhook retry issues", `1 result\n${REPO}#2241 Webhook retry attempt 6 fires after 2 h instead of 8 h — open · sev-2 · webhooks · no assignee · opened 2026-08-11`),
        ],
        text: `Duplicate of ${REPO}#2241 — same symptom (attempt 6 at the attempt-5 delay), open since Aug 11 and still unassigned. No new issue; the receipt tells helpdesk to point ZD-5081 at #2241.`,
      },
    ],
  }).id;
  x3.envelope.runId = appendScriptedRun(store, session, {
    at: digestAt,
    user: x3.prompt,
    steps: [
      {
        tools: [
          github("search_issues", { q: `repo:${REPO} is:issue label:from-support created:>=${weekStart}` }, "filed this week", `3 results\n#2263 POST /oauth/token 500 on trailing-slash redirect_uri — sev-3 · auth · assignee priya\n#2270 SDK (Python) subscribe() drops events over 1 MiB — sev-2 · sdk · assignee priya\n#2274 Dashboard attempt log shows UTC as local — sev-3 · dashboard · no assignee`),
          github("search_issues", { q: `repo:${REPO} is:issue label:from-support is:closed closed:>=${weekStart}` }, "closed this week", `2 results\n#2238 Bulk export CSV header row missing — closed 2 days ago (fixed in 2026.08.20-1)\n#2251 401 on key rotation overlap — closed 5 days ago (docs)`),
          github("search_issues", { q: `repo:${REPO} is:issue is:open label:sev-2 no:assignee` }, "sev-2 without assignee", `1 result\n#2241 Webhook retry attempt 6 fires after 2 h instead of 8 h — opened 2026-08-11 (6 business days)`),
        ],
      },
      {
        tools: [
          github("add_issue_comment", { owner: "northwind", repo: "platform", issue_number: 2200, body: `## Weekly bug digest (week of ${weekStart})\n\n**Filed from support:** #2263 (sev-3, auth), #2270 (sev-2, sdk), #2274 (sev-3, dashboard)\n**Closed:** #2238, #2251\n**Needs an owner:** #2241 (sev-2, webhooks) — open 6 business days without an assignee. @northwind/platform-leads` }, "digest on #2200", `comment posted https://github.com/northwind/platform/issues/2200#issuecomment-${hex(`digest:${weekStart}`, 10)}`),
        ],
        text: "Digest posted on #2200: three filed this week (#2263, #2270, #2274), two closed (#2238, #2251), and #2241 named to leads — sev-2, six business days, no assignee.",
      },
    ],
  }).id;
  x4.envelope.runId = appendScriptedRun(store, session, {
    at: ago(2 * HOUR_MS + 41 * MINUTE_MS),
    user: x4.prompt,
    steps: [
      {
        thinking: "Cursor pagination inclusive at the boundary, sandbox-only build, four request ids and a curl repro — complete. The matrix changed since I last read it (KB revision 31); re-read the severity table, then search before filing.",
        tools: [
          vfsReadFile("/kb/escalation-matrix.md", KB_ESCALATION),
          github("search_issues", { q: `repo:${REPO} is:issue events cursor pagination duplicate after` }, "duplicates?", `2 results, both closed\n#2118 Events cursor expires after 24 h — closed 2026-07-30 (different symptom)\n#1907 Offset pagination skips items under writes — closed 2025-11-04 (v2 list; replaced by cursors)`),
        ],
      },
      {
        tools: [
          github(
            "create_issue",
            { owner: "northwind", repo: "platform", title: "GET /v3/events: `after` cursor is inclusive at every page boundary (sandbox 2026.08.27-3)", labels: ["sev-2", "api-events", "from-support"], body: ISSUE_2287_BODY },
            "sev-2 · api-events",
            `created ${ISSUE_2287} https://github.com/northwind/platform/issues/2287`,
          ),
        ],
        text: `Filed ${ISSUE_2287} (sev-2, api-events, from-support): expected an exclusive cursor, actual inclusive at every boundary on sandbox build 2026.08.27-3, curl repro with all four request ids, ZD-5107. Not a duplicate — #2118 was cursor expiry. Helpdesk gets the receipt with the issue number when this run finishes.`,
      },
    ],
  }).id;

  const state = botState({
    bot: record,
    sessions: [botSession(session, "main")],
    recentEvents: [
      recent(x4.envelope, { inputTokens: 18_420, cachedInputTokens: 16_930 }),
      recent(x3.envelope, { inputTokens: 16_110, cachedInputTokens: 15_240 }),
      recent(x2.envelope, { inputTokens: 13_870, cachedInputTokens: 12_900 }),
      recent(x1.envelope, { inputTokens: 12_450, cachedInputTokens: 9_360 }),
    ],
    eventsProcessed: 23,
    appliedProfileRevision: ESCALATION_PROFILE.revision,
    runsToday: 1,
  });
  universe.bots.set(BOT.escalations, { bot: record, triggers, events: log.events, state, descendants: [] });
}

// ---------------------------------------------------------------------------
// Status watch and on-call: the incident pair
// ---------------------------------------------------------------------------

interface Incident {
  id: string;
  name: string;
  status: string;
  impact: string;
  shortlink: string;
  components: string[];
  body: string;
}

const DELAY_INCIDENT: Incident = {
  id: "inc_7d21",
  name: "Webhook delivery delays (EU)",
  status: "investigating",
  impact: "major",
  shortlink: `${STATUS_PAGE_URL}/incidents/inc_7d21`,
  components: ["Webhooks"],
  body: "Webhook deliveries for EU-region accounts are delayed. Deliveries are queued and will be sent; we are investigating.",
};
const SANDBOX_INCIDENT: Incident = {
  id: "inc_7f0a",
  name: "Elevated error rate on the sandbox API (eu-1)",
  status: "investigating",
  impact: "minor",
  shortlink: `${STATUS_PAGE_URL}/incidents/inc_7f0a`,
  components: ["Sandbox API"],
  body: "A subset of sandbox API requests in eu-1 return 5xx. Production is not affected.",
};

const STATUS_WATCH_BRIEF = `Poll items are status-page incidents. Page oncall (bot_emit to oncall, kind incident.open, reply: true) for anything touching the API, webhook delivery, or auth while the status is investigating or identified; sandbox-only incidents are severity 3 — note and move on. One page per incident; send an incident.update only when the impact changes, including when it clears.`;

const ONCALL_BRIEF = `Primary on call this week: Jonas (Europe/Berlin). Acknowledge pages with pagerduty.acknowledge, open #inc-<mmdd> for severity 1–2 with the slack CLI in the support-tools environment, and brief helpdesk when developers will notice — and again with the all-clear. Do not resolve incidents; humans do that. Known: the PagerDuty API token has been rejected since Aug 24 (#4); Jonas is re-issuing it — note pages in #ops instead of acknowledging until then.`;

function seedStatusWatch(store: DemoStore, universe: UniverseState): void {
  const record = bot(universe, {
    botId: BOT.statusWatch,
    displayName: "Status watch",
    description: "Watches the public status page and raises incidents with on-call.",
    profileId: PROFILE.triage,
    brief: STATUS_WATCH_BRIEF,
    runsPerDay: 200,
    breaker: null,
    emit: true,
    createdAtMs: ago(30 * DAY_MS),
    updatedAtMs: ago(9 * DAY_MS),
  });
  const triggers = new Map([
    [
      "statuspage-poll",
      pollTrigger(
        BOT.statusWatch,
        "statuspage-poll",
        {
          source: {
            kind: "http",
            url: STATUS_FEED_URL,
            method: "GET",
            headers: { accept: "application/json" },
            auth: { grantId: GRANT.statuspage, header: "authorization", scheme: "Bearer" },
          },
          intervalMs: 2 * MINUTE_MS,
          items: "$.incidents[*]",
          cursor: { kind: "watermark", field: "updated_at" },
        },
        {
          filter: 'data.status != "resolved"',
          route: { policy: "bot" },
          coalesce: { debounceMs: 45_000, maxWaitMs: 2 * MINUTE_MS, maxCount: 10 },
          deliver: { whenBusy: "queue" },
          cursorState: {
            watermark: agoIso(65 * MINUTE_MS),
            consecutiveFailures: 0,
            baselinedAtMs: ago(30 * DAY_MS),
            lastPolledAtMs: ago(80_000),
          },
          createdAtMs: ago(30 * DAY_MS),
          updatedAtMs: ago(9 * DAY_MS),
        },
      ),
    ],
  ]);

  const log = eventLog(store, BOT.statusWatch);
  const pollEvent = (incident: Incident, atMs: number, outcome: "handled" | "ignored", detail: string): ScriptedEvent =>
    log.add({
      kind: "poll",
      source: "poll:statuspage-poll",
      at: atMs,
      summary: `${incident.name} — ${incident.status} (${incident.impact}): ${incident.body}`,
      eventId: `poll:statuspage-poll:${incident.id}@${atIso(atMs)}`,
      session: main(BOT.statusWatch),
      outcome,
      detail,
      data: { ...incident, updated_at: atIso(atMs) },
    });

  const s1 = pollEvent(DELAY_INCIDENT, at(9, 14, 4), "handled", `Severity 2: paged oncall (#${SEQ.oncallIncident} there), receipt requested`);
  const s2 = log.add({
    kind: "bot.reply",
    source: `bot:${BOT.oncall}`,
    at: at(9, 14, 9),
    summary: `#${SEQ.oncallIncident} at ${BOT.oncall} finished handled: Paged primary (Jonas), PD-4482 acknowledged; #inc-0819 opened; helpdesk briefed`,
    eventId: `reply:${BOT.oncall}:${hex(`dlv-${BOT.oncall}-${SEQ.oncallIncident}\nincident.open:${DELAY_INCIDENT.id}`, 24)}`,
    session: main(BOT.statusWatch),
    sender: BOT.oncall,
    hops: 2,
    inReplyTo: { bot: BOT.oncall, seq: SEQ.oncallIncident },
    outcome: "handled",
    detail: "On-call has it (#inc-0819); nothing further until the impact changes",
    data: { status: "handled" },
  });
  const s3 = pollEvent({ ...DELAY_INCIDENT, status: "identified", body: "A delivery-worker build tripled CPU per delivery; a rollback is in progress and the EU backlog is draining." }, at(9, 14, 42), "handled", "Two updates in one delivery: identified, then the drain ETA — impact unchanged, no new page");
  const s4 = pollEvent({ ...DELAY_INCIDENT, status: "identified", body: "Backlog draining at about 14 k deliveries/s; ETA for a clean queue 15:15." }, at(9, 14, 43), "handled", "Two updates in one delivery: identified, then the drain ETA — impact unchanged, no new page");
  const s5 = pollEvent({ ...DELAY_INCIDENT, status: "monitoring", body: "The backlog has drained and deliveries are current. We are monitoring." }, at(9, 15, 20), "handled", `Monitoring, backlog gone — impact changed, told oncall (incident.update, #${SEQ.oncallUpdate} there)`);
  const s6 = pollEvent(
    { id: "inc_7e55", name: "Scheduled maintenance: sandbox database upgrade", status: "scheduled", impact: "maintenance", shortlink: `${STATUS_PAGE_URL}/incidents/inc_7e55`, components: ["Sandbox API"], body: "The sandbox database is upgraded on Aug 30, 02:00–02:30 UTC, right after the weekly reset. No production impact." },
    at(2, 9, 30),
    "ignored",
    "Scheduled maintenance; not an incident",
  );
  const s7 = pollEvent(SANDBOX_INCIDENT, ago(HOUR_MS + 50 * MINUTE_MS), "handled", "Severity 3: sandbox only, 2.3% of requests — logged, no page");
  const s8 = pollEvent({ ...SANDBOX_INCIDENT, status: "monitoring", body: "The affected sandbox nodes were rotated out; error rates are back to baseline." }, ago(HOUR_MS + 5 * MINUTE_MS), "handled", "Sandbox back to baseline; nothing for on-call");

  const session = managedSession(store, universe, {
    id: main(BOT.statusWatch).sessionId,
    botId: BOT.statusWatch,
    displayName: "Status watch",
    profile: TRIAGE_PROFILE,
    tools: [...BOT_TOOLS, EMIT_TOOL],
    createdAtMs: ago(30 * DAY_MS),
    environmentId: ENV_SUPPORT_TOOLS,
  });
  s1.envelope.runId = appendScriptedRun(store, session, {
    at: at(9, 14, 4),
    user: s1.prompt,
    steps: [
      {
        thinking: "Webhooks, investigating, impact major — severity 2 by the brief: every EU developer with an endpoint notices. Confirm the detail, then page on-call with a receipt requested so I know it landed.",
        tools: [webFetch(DELAY_INCIDENT.shortlink, "incident detail", JSON.stringify({ ...DELAY_INCIDENT, started_at: atIso(at(9, 13, 55)), queue_depth: 1_870_000, delivery_lag_p95_s: 214 }, null, 2))],
      },
      {
        tools: [
          botEmit(
            {
              to: BOT.oncall,
              kind: "incident.open",
              summary: `Webhook delivery delays (EU) — investigating, impact major. Queue depth 1.87 M, delivery lag p95 214 s since 13:55. Severity 2: page primary. ${DELAY_INCIDENT.shortlink}`,
              data: { incident: DELAY_INCIDENT.id, severity: 2, components: DELAY_INCIDENT.components },
              reply: true,
            },
            SEQ.oncallIncident,
          ),
        ],
        text: `Severity 2 — 1.87 M deliveries queued, lag p95 214 s. Paged oncall (their #${SEQ.oncallIncident}) with a receipt requested. No second page unless the impact changes.`,
      },
    ],
  }).id;
  s2.envelope.runId = appendScriptedRun(store, session, {
    at: at(9, 14, 9),
    user: s2.prompt,
    steps: [{ text: "On-call acknowledged PD-4482, opened #inc-0819, and briefed helpdesk. Nothing further from me until the incident's impact changes." }],
  }).id;
  const batchRun = appendScriptedRun(store, session, {
    at: at(9, 14, 43) + 45_000,
    user: `${s3.prompt}\n\n${s4.prompt}`,
    steps: [{ text: "Both updates are the delay incident on-call already has: identified (a worker build tripled CPU per delivery, rollback running) and a drain ETA of 15:15. Impact did not change, so no new page." }],
  });
  s3.envelope.runId = batchRun.id;
  s4.envelope.runId = batchRun.id;
  s5.envelope.runId = appendScriptedRun(store, session, {
    at: at(9, 15, 20),
    user: s5.prompt,
    steps: [
      {
        tools: [
          botEmit(
            {
              to: BOT.oncall,
              kind: "incident.update",
              summary: `Webhook delivery delays (EU) — monitoring: the backlog drained at 15:20 and deliveries are current. Impact cleared; developers need the all-clear. ${DELAY_INCIDENT.shortlink}`,
              data: { incident: DELAY_INCIDENT.id, severity: 2, status: "monitoring" },
            },
            SEQ.oncallUpdate,
          ),
        ],
        text: `Backlog gone, status monitoring — the impact changed, so on-call gets one update (their #${SEQ.oncallUpdate}) to send helpdesk the all-clear. Resolution is theirs.`,
      },
    ],
  }).id;
  s6.envelope.runId = appendScriptedRun(store, session, {
    at: at(2, 9, 30),
    user: s6.prompt,
    steps: [{ text: "Scheduled maintenance on the sandbox during its weekly reset window, no production impact. Not an incident; ignoring." }],
  }).id;
  s7.envelope.runId = appendScriptedRun(store, session, {
    at: ago(HOUR_MS + 50 * MINUTE_MS),
    user: s7.prompt,
    steps: [
      {
        tools: [webFetch(SANDBOX_INCIDENT.shortlink, "incident detail", JSON.stringify({ ...SANDBOX_INCIDENT, affected_regions: ["eu-1"], error_rate: "2.3%", production_error_rate: "0.02%" }, null, 2))],
        text: "Severity 3: sandbox only, one region, 2.3% of requests, production at baseline. Logged; no page.",
      },
    ],
  }).id;
  s8.envelope.runId = appendScriptedRun(store, session, {
    at: ago(HOUR_MS + 5 * MINUTE_MS),
    user: s8.prompt,
    steps: [{ text: "Same sandbox incident, now monitoring with error rates at baseline. Nothing for on-call." }],
  }).id;

  const state = botState({
    bot: record,
    sessions: [botSession(session, "main")],
    recentEvents: [
      recent(s8.envelope, { inputTokens: 9_340, cachedInputTokens: 8_970 }),
      recent(s7.envelope, { inputTokens: 10_020, cachedInputTokens: 9_380 }),
      recent(s6.envelope, { inputTokens: 8_900, cachedInputTokens: 8_410 }),
      recent(s5.envelope, { inputTokens: 9_610, cachedInputTokens: 9_050 }),
    ],
    eventsProcessed: 3_118,
    appliedProfileRevision: TRIAGE_PROFILE.revision,
    runsToday: 137,
  });
  universe.bots.set(BOT.statusWatch, { bot: record, triggers, events: log.events, state, descendants: [] });
}

interface Page {
  id: string;
  title: string;
  urgency: "high" | "low";
  service: string;
}

function seedOncall(store: DemoStore, universe: UniverseState): void {
  const record = bot(universe, {
    botId: BOT.oncall,
    displayName: "On-call",
    description: "Acknowledges pages, opens incident channels, and briefs the helpdesk.",
    profileId: PROFILE.triage,
    brief: ONCALL_BRIEF,
    runsPerDay: null,
    breaker: { fires: 20, windowMs: 10 * MINUTE_MS },
    emit: true,
    createdAtMs: ago(30 * DAY_MS),
    updatedAtMs: ago(2 * DAY_MS),
  });
  const triggers = new Map([
    ["inbox", inboxTrigger(BOT.oncall, [BOT.statusWatch], { route: { policy: "bot" }, deliver: { whenBusy: "steer" }, createdAtMs: ago(30 * DAY_MS) })],
    [
      "pagerduty",
      webhookTrigger(
        universe,
        BOT.oncall,
        "pagerduty",
        {
          token: "whk_9f3c2a71d0e84b5f",
          verification: { scheme: "hmac-sha256", grantId: GRANT.pagerdutySigning, header: "x-pagerduty-signature", prefix: "v1=" },
          preset: null,
        },
        {
          filter: 'data.event.event_type in ["incident.triggered", "incident.escalated"]',
          route: { policy: "bot" },
          deliver: { whenBusy: "steer" },
          createdAtMs: ago(29 * DAY_MS),
        },
      ),
    ],
    [
      "daily-digest",
      scheduleTrigger(
        BOT.oncall,
        "daily-digest",
        { cron: "0 8 * * *", summary: "Post yesterday's incident digest to #ops: every page, who acknowledged it, and any postmortem action item due this week." },
        {
          route: { policy: "bot" },
          deliver: { whenBusy: "queue" },
          enabled: false,
          disabledReason: "operator",
          disabledAtMs: ago(2 * DAY_MS),
          createdAtMs: ago(28 * DAY_MS),
          updatedAtMs: ago(2 * DAY_MS),
        },
      ),
    ],
  ]);

  const log = eventLog(store, BOT.oncall);
  const page = (incident: Page, atMs: number, outcome: "handled" | "steered" | "run_failed", detail: string): ScriptedEvent =>
    log.add({
      kind: "incident.triggered",
      source: "webhook:pagerduty",
      at: atMs,
      summary: `Webhook incident.triggered received on trigger pagerduty: ${incident.id} ${incident.title} (${incident.urgency} urgency, ${incident.service})`,
      eventId: `webhook:pagerduty:${incident.id}:triggered`,
      session: main(BOT.oncall),
      outcome,
      detail,
      data: {
        event: {
          event_type: "incident.triggered",
          data: { id: incident.id, title: incident.title, urgency: incident.urgency, service: { summary: incident.service }, html_url: `https://northwind.pagerduty.com/incidents/${incident.id}` },
        },
      },
    });

  const o1 = log.add({
    kind: "incident.open",
    source: `bot:${BOT.statusWatch}`,
    at: at(9, 14, 5),
    summary: `Webhook delivery delays (EU) — investigating, impact major. Queue depth 1.87 M, delivery lag p95 214 s since 13:55. Severity 2: page primary. ${DELAY_INCIDENT.shortlink}`,
    eventId: `bot:${BOT.statusWatch}:${hex(`incident.open:${DELAY_INCIDENT.id}`, 24)}`,
    session: main(BOT.oncall),
    sender: BOT.statusWatch,
    hops: 1,
    outcome: "handled",
    detail: `Paged primary (Jonas), PD-4482 acknowledged; #inc-0819 opened; helpdesk briefed (#${SEQ.helpdeskNotice} there)`,
    resolvedAfterMs: 3 * MINUTE_MS,
    data: { incident: DELAY_INCIDENT.id, severity: 2, components: DELAY_INCIDENT.components },
  });
  const o2 = page({ id: "PD-4482", title: "Webhook delivery lag p95 above 120 s (eu-1)", urgency: "high", service: "Webhooks" }, at(9, 14, 6), "steered", `Steered into the run for #${SEQ.oncallIncident}: PagerDuty's page for the same incident`);
  const o3 = log.add({
    kind: "incident.update",
    source: `bot:${BOT.statusWatch}`,
    at: at(9, 15, 21),
    summary: `Webhook delivery delays (EU) — monitoring: the backlog drained at 15:20 and deliveries are current. Impact cleared; developers need the all-clear. ${DELAY_INCIDENT.shortlink}`,
    eventId: `bot:${BOT.statusWatch}:${hex(`incident.update:${DELAY_INCIDENT.id}:monitoring`, 24)}`,
    session: main(BOT.oncall),
    sender: BOT.statusWatch,
    hops: 1,
    outcome: "handled",
    detail: `All-clear sent to helpdesk (#${SEQ.helpdeskAllClear} there) with the 15:03–15:20 duplicate window; #inc-0819 left open for Jonas to resolve`,
    data: { incident: DELAY_INCIDENT.id, severity: 2, status: "monitoring" },
  });
  const o4 = page({ id: "PD-4497", title: "API 5xx rate above 1% (eu-1)", urgency: "high", service: "API" }, at(4, 11, 48), "run_failed", "run failed: tool pagerduty.acknowledge returned 401 Unauthorized — the PagerDuty API token was rejected (rotated 2026-08-24)");
  const o5 = log.add({
    kind: "schedule",
    source: "schedule:daily-digest",
    at: at(3, 8, 0),
    summary: "Post yesterday's incident digest to #ops: every page, who acknowledged it, and any postmortem action item due this week.",
    eventId: `schedule:daily-digest:${atIso(at(3, 8, 0))}`,
    session: main(BOT.oncall),
    outcome: "handled",
    detail: "Posted the digest to #ops: 1 page (PD-4497, acknowledged by Jonas — my token is still rejected), 2 postmortem items due this week",
    data: { trigger: "daily-digest", cron: "0 8 * * *", timezone: "Europe/Berlin", scheduledAt: atIso(at(3, 8, 0)) },
  });
  const o6 = page({ id: "PD-4510", title: "TLS certificate for webhooks.northwind.dev expires in 7 days", urgency: "low", service: "Webhooks" }, ago(2 * HOUR_MS + 50 * MINUTE_MS), "handled", "Low urgency: auto-renewal is scheduled for Sep 1; not acknowledged — PagerDuty token still rejected (#4) — noted for Jonas in #ops");

  const session = managedSession(store, universe, {
    id: main(BOT.oncall).sessionId,
    botId: BOT.oncall,
    displayName: "On-call",
    profile: TRIAGE_PROFILE,
    tools: [...BOT_TOOLS, EMIT_TOOL],
    createdAtMs: ago(30 * DAY_MS),
    environmentId: ENV_SUPPORT_TOOLS,
  });
  o1.envelope.runId = appendScriptedRun(store, session, {
    at: at(9, 14, 5),
    user: o1.prompt,
    steps: [
      {
        thinking: "Severity 2 from status-watch: webhook delivery for every EU account is lagging. Confirm the status-page detail, then page Jonas, open the incident channel, and brief helpdesk — developers will ask within minutes whether deliveries are lost.",
        tools: [webFetch(DELAY_INCIDENT.shortlink, "incident detail", JSON.stringify({ ...DELAY_INCIDENT, started_at: atIso(at(9, 13, 55)), queue_depth: 1_870_000, delivery_lag_p95_s: 214, dropped: 0 }, null, 2))],
      },
      {
        tools: [
          mcpCall("pagerduty.acknowledge", { incidentId: "PD-4482" }, { id: "PD-4482", status: "acknowledged", acknowledgedBy: "oncall-bot", assignedTo: "Jonas Lindqvist", escalationPolicy: "API" }),
          runCommand(["slack", "channel", "create", "#inc-0819", "--topic", "Webhook delivery delays (EU) — sev 2 — PD-4482"], "created #inc-0819 · invited: jonas, sofia, priya", { detail: ENV_SUPPORT_TOOLS, cwd: "/home/support" }),
        ],
      },
      {
        tools: [
          botEmit(
            { to: BOT.helpdesk, kind: "incident.notice", summary: DELAY_NOTICE, data: { incident: "#inc-0819", severity: 2, until: "15:00" } },
            SEQ.helpdeskNotice,
          ),
        ],
        text: `PagerDuty paged Jonas for the same incident mid-run (PD-4482) — acknowledged it, nothing separate to do. Opened #inc-0819 with Sofia and Priya. Helpdesk has the developer wording (their #${SEQ.helpdeskNotice}) until 15:00: queued, not dropped, dedupe by delivery id. status-watch gets the receipt when this run finishes.`,
      },
    ],
    steer: { afterStep: 1, text: o2.prompt },
  }).id;
  o3.envelope.runId = appendScriptedRun(store, session, {
    at: at(9, 15, 21),
    user: o3.prompt,
    steps: [
      {
        tools: [
          botEmit(
            { to: BOT.helpdesk, kind: "incident.notice", summary: ALL_CLEAR_NOTICE, data: { incident: "#inc-0819", severity: 2, resolved: true } },
            SEQ.helpdeskAllClear,
          ),
        ],
        text: `All-clear to helpdesk (their #${SEQ.helpdeskAllClear}), with the 212 duplicate deliveries from the shard-3 re-send called out so the dedupe advice lands. #inc-0819 stays open; Jonas resolves.`,
      },
    ],
  }).id;
  o4.envelope.runId = appendScriptedRun(store, session, {
    at: at(4, 11, 48),
    user: o4.prompt,
    steps: [
      {
        thinking: "5xx above 1% on the production API is severity 2 by the brief. Acknowledge first, then the status page.",
        tools: [pagerdutyFailure("acknowledge", { incidentId: "PD-4497" }, "401 Unauthorized: the API token was rejected by PagerDuty (token rotated 2026-08-24). Re-enter the token for grant grant-pagerduty-api.")],
      },
    ],
    failure: "tool pagerduty.acknowledge returned 401 Unauthorized — the PagerDuty API token was rejected (rotated 2026-08-24)",
  }).id;
  o5.envelope.runId = appendScriptedRun(store, session, {
    at: at(3, 8, 0),
    user: o5.prompt,
    steps: [
      {
        tools: [
          mcpCall("pagerduty.list_incidents", { since: "yesterday", statuses: ["triggered", "acknowledged", "resolved"] }, [{ id: "PD-4497", title: "API 5xx rate above 1% (eu-1)", status: "resolved", acknowledgedBy: "Jonas Lindqvist", resolvedAt: "12:10" }]),
          webFetch(STATUS_FEED_URL, "status page", JSON.stringify({ incidents: [] }, null, 2)),
        ],
      },
      {
        tools: [runCommand(["slack", "post", "#ops", "--file", "/tmp/digest.md"], "posted: https://northwind.slack.com/archives/C0OPS/p1756360800", { detail: ENV_SUPPORT_TOOLS, cwd: "/home/support" })],
        text: "Digest posted to #ops: one page yesterday (PD-4497, acknowledged by Jonas because my PagerDuty token is still rejected — see #4, resolved 12:10, a bad node rotated out); status page clear; postmortem items due this week: #2 canary (Jonas, Sep 2) and #4 status-page template (Priya, Aug 29).",
      },
    ],
  }).id;
  o6.envelope.runId = appendScriptedRun(store, session, {
    at: ago(2 * HOUR_MS + 50 * MINUTE_MS),
    user: o6.prompt,
    steps: [
      {
        thinking: "Low urgency: a certificate expiry warning, and the dashboard shows auto-renewal on Sep 1. The PagerDuty token has been rejected since Aug 24 (#4), so acknowledging would fail again — note it in #ops as the brief says and leave the page for Jonas.",
        tools: [runCommand(["slack", "post", "#ops", "PD-4510: webhooks.northwind.dev certificate expires in 7 days; auto-renewal scheduled Sep 1 02:00 UTC. Not acknowledged — PagerDuty token still rejected. @jonas"], "posted: https://northwind.slack.com/archives/C0OPS/p1756456200", { detail: ENV_SUPPORT_TOOLS, cwd: "/home/support" })],
        text: "Low urgency, auto-renewal on Sep 1. Noted in #ops for Jonas instead of acknowledging — the PagerDuty token is still the one rejected in #4. Nothing for helpdesk.",
      },
    ],
  }).id;

  const state = botState({
    bot: record,
    sessions: [botSession(session, "main")],
    recentEvents: [
      recent(o6.envelope, { inputTokens: 10_480, cachedInputTokens: 9_910 }),
      recent(o5.envelope, { inputTokens: 11_020, cachedInputTokens: 10_300 }),
      recent(o4.envelope),
      recent(o3.envelope, { inputTokens: 12_610, cachedInputTokens: 11_950 }),
    ],
    eventsProcessed: 96,
    appliedProfileRevision: TRIAGE_PROFILE.revision,
    runsToday: 1,
  });
  universe.bots.set(BOT.oncall, { bot: record, triggers, events: log.events, state, descendants: [] });
}

// ---------------------------------------------------------------------------
// Interactive sessions
// ---------------------------------------------------------------------------

const WEEK34_DIGEST = `# Developer support digest — week 34 (Aug 17–23)

| | Week 34 | Week 33 |
| --- | --- | --- |
| Conversations (Telegram + WhatsApp) | 143 | 118 |
| Tickets opened | 61 | 47 |
| Median first response | 4 min | 5 min |
| Escalations to engineering | 5 | 2 |
| Issues filed | 3 | 2 |

**Top issues**
| Topic | Tickets | KB page | Note |
| --- | --- | --- | --- |
| Webhook delivery delays (Aug 19 incident) | 27 | /kb/webhook-delivery.md | 26 answered from the incident notice, 1 ticket |
| Signature verification fails | 9 | /kb/webhook-delivery.md | 7 of 9 hashed a re-serialized body |
| SDK v3: \`events.on\` removed | 8 | /kb/sdk-v3-migration.md | codemod sent every time |
| 429 on the events list | 6 | /kb/rate-limits.md | 5 of 6 were polling instead of webhooks |
| Duplicate deliveries after the incident | 4 | — | **no KB page** — postmortem action item 5 |
| 401 \`environment_mismatch\` | 3 | /kb/api-authentication.md | sandbox key on production |

**What changed**
- The Aug 19 incident doubled conversation volume for two days; the helpdesk bot handled it with one ticket because on-call's notice reached its brief six minutes in.
- Escalations went from 2 to 5: three were real (#2263, #2270, #2274), two were duplicates of #2241 — which is sev-2, unassigned since Aug 11, and now named in the Friday digest to leads.
- SDK v3 tickets are flat week over week (8 vs 7); the codemod answers them, but they will not stop until v2 warns louder at startup.

**Open action items**
- Postmortem #2 canary on CPU per delivery — Jonas, Sep 2
- Postmortem #4 status-page template — Priya, Aug 29
- Postmortem #5 idempotency KB article — Priya, Sep 5
- Assign #2241 — platform leads, this week

Missing: CSAT for the week is not in Zendesk yet — left out rather than guessed.`;

const WEEK34_FOLLOW_UP = `Two changes, one of them not a new page.

1. **Duplicate deliveries / idempotency — new page.** Four tickets, no page, and the postmortem already owns it (action item 5, Priya, Sep 5). The page should cover both directions: \`Idempotency-Key\` on POST requests and deduplicating inbound deliveries by \`X-Northwind-Delivery-Id\`, with a pointer from /kb/webhook-delivery.md. Support can draft it; Priya reviews.
2. **Signature failures — a callout, not a page.** Seven of nine were the same mistake (hashing a re-serialized body), so the fix is one line at the top of /kb/webhook-delivery.md — "hash the raw request bytes" — and the same line in the public docs under Verifying deliveries.

The rest already have pages that answer them. The SDK v3 volume is a product problem (the v2 startup warning), not a KB one — worth raising with Priya rather than writing more.`;

const POSTMORTEM_ANSWER = `Four of five action items are still open.

| # | Action | Owner | Due | Status |
| --- | --- | --- | --- | --- |
| 2 | Canary on CPU-seconds per delivery | Jonas Lindqvist | Sep 2 | open |
| 3 | Rollback drains in-flight acks before cutting over | Sofia Brandt | Sep 9 | open |
| 4 | Status-page template says "queued, not dropped" | Priya Natarajan | Aug 29 | open — **due first** |
| 5 | KB article on idempotency | Priya Natarajan | Sep 5 | open |

Item 1 (sign the stored bytes, byte-identical test) is done.

**Chase this week**
1. **#4 with Priya** — due Aug 29 and the cheapest: it is a status-page template. It would have removed the six minutes between "delayed" and "not dropped" reaching helpdesk.
2. **#2 with Jonas** — due Sep 2 and it is the detection fix; the canary measured the wrong thing and a repeat looks the same until this lands.
3. **#5 with Priya** — not urgent, but it is the page the digest keeps asking for (four "duplicate deliveries" tickets last week with nothing to link). Support could draft it and hand Priya a review instead of a blank page.

#3 can wait for Sofia's rotation; it depends on nothing above.`;

function seedInteractiveSessions(store: DemoStore, universe: UniverseState): void {
  const digest = newSession(store, universe, {
    id: "session-ts-weekly-digest",
    displayName: "Weekly support digest — week 34",
    config: structuredClone(DIGEST_PROFILE.config),
    instructions: DIGEST_PROFILE.instructions,
    createdAtMs: at(3, 9, 15),
  });
  appendScriptedRun(store, digest, {
    at: at(3, 9, 15),
    user: "Draft the week 34 digest for Monday's support review. Top issues as a table with the KB page for each, and the escalation count.",
    steps: [
      {
        thinking: "One page, numbers first. Escalation definitions come from the matrix, incident context from the postmortem, counts from Zendesk. Do not invent CSAT if it is not there.",
        tools: [
          vfsReadFile("/kb/escalation-matrix.md", KB_ESCALATION),
          vfsReadFile("/postmortems/2026-08-19-webhook-delivery-delays.md", POSTMORTEM_DELAYS),
          mcpCall("zendesk.get_ticket_metrics", { period: "2026-W34", groupBy: "topic" }, {
            period: "2026-W34",
            conversations: 143,
            tickets: 61,
            medianFirstResponseMin: 4,
            escalations: 5,
            topics: [
              { topic: "webhook-delivery-delay", tickets: 27 },
              { topic: "webhook-signature", tickets: 9 },
              { topic: "sdk-v3", tickets: 8 },
              { topic: "rate-limit", tickets: 6 },
              { topic: "duplicate-deliveries", tickets: 4 },
              { topic: "auth-environment-mismatch", tickets: 3 },
            ],
            previous: { conversations: 118, tickets: 47, medianFirstResponseMin: 5, escalations: 2 },
            csat: null,
          }),
        ],
        text: WEEK34_DIGEST,
      },
    ],
  });
  appendScriptedRun(store, digest, {
    at: at(3, 9, 40),
    user: "Which of the top issues should become KB articles?",
    steps: [{ text: WEEK34_FOLLOW_UP }],
  });

  const postmortem = newSession(store, universe, {
    id: "session-ts-postmortem",
    displayName: "Postmortem: 2026-08-19 webhook delivery delays",
    config: structuredClone(DIGEST_PROFILE.config),
    instructions: DIGEST_PROFILE.instructions,
    createdAtMs: ago(6 * HOUR_MS),
  });
  appendScriptedRun(store, postmortem, {
    at: ago(6 * HOUR_MS),
    user: "Read the webhook delivery postmortem and tell me which action items are still open, who owns them, and what I should chase this week.",
    steps: [
      {
        thinking: "Read the write-up first; the action-item table has owners and due dates, so the answer is a filter on status and date, plus which ones actually reduce a repeat.",
        tools: [vfsReadFile("/postmortems/2026-08-19-webhook-delivery-delays.md", POSTMORTEM_DELAYS)],
        text: POSTMORTEM_ANSWER,
      },
    ],
  });

  const article = newSession(store, universe, {
    id: "session-ts-kb-idempotency",
    displayName: "Draft KB article: idempotency keys",
    config: structuredClone(KB_AUTHOR_CONFIG),
    instructions: SUPPORT_INSTRUCTIONS,
    createdAtMs: ago(HOUR_MS + 40 * MINUTE_MS),
  });
  appendScriptedRun(store, article, {
    at: ago(HOUR_MS + 40 * MINUTE_MS),
    user: "Draft /kb/idempotency-keys.md for the developer KB: Idempotency-Key on POST /v3/events (header, replay window, what a replay returns, key scope) and the inbound side (dedupe by X-Northwind-Delivery-Id) with a pointer to the webhook page. Match the KB style, support rules at the end.",
    steps: [
      {
        thinking: "Match the existing pages: a one-line lead, sections with tables or short lists, a code block where a snippet saves a paragraph, and Support rules last. The webhook page already has the inbound half; the new page should point at it rather than repeat it.",
        tools: [
          vfsReadFile("/kb/webhook-delivery.md", KB_WEBHOOKS),
          vfsReadFile("/kb/api-authentication.md", KB_AUTH),
        ],
      },
      {
        tools: [vfsWriteFile("/kb/idempotency-keys.md", KB_IDEMPOTENCY, "new page, KB style")],
        text: "Drafted /kb/idempotency-keys.md: the outbound `Idempotency-Key` contract (24 h replay window, same-body replay returns the stored response with `Idempotent-Replayed: true`, different body is `409 idempotency_key_reused`, scoped per key and environment), the SDK default with a one-line snippet, the inbound half as a pointer to /kb/webhook-delivery.md, three common mistakes, and support rules. Two things to confirm before Priya reviews: the 128-character key limit and whether `/v3/exports` honours the header — both are from the reference, not verified against the sandbox.",
      },
    ],
  });
}

// ---------------------------------------------------------------------------
// Live responder
// ---------------------------------------------------------------------------

const STATUS_FEED_NOW = JSON.stringify(
  {
    page: { name: "Northwind", url: STATUS_PAGE_URL, updated_at: agoIso(HOUR_MS + 5 * MINUTE_MS) },
    incidents: [
      { ...SANDBOX_INCIDENT, status: "monitoring", body: "The affected sandbox nodes were rotated out; error rates are back to baseline.", updated_at: agoIso(HOUR_MS + 5 * MINUTE_MS) },
      { ...DELAY_INCIDENT, status: "resolved", impact: "none", updated_at: atIso(at(9, 15, 45)) },
    ],
  },
  null,
  2,
);

function webhookTurn(turn: number): DemoTurn {
  if (turn > 1) {
    return {
      text: "Same recipe as above, in short: sign `{timestamp}.{raw_body}` with the endpoint's `whsec_…` secret, hash the raw request bytes (not a re-serialized object), reject anything more than 300 s from now, and compare with a constant-time function.\n\nIf you paste one delivery id (`X-Northwind-Delivery-Id`, `dlv_…`) I will pull the attempt from the log and check what we signed against what you received.",
    };
  }
  return {
    thinking: "A signature or delivery question. The KB has the exact algorithm and the retry table — read it before answering so the numbers are right.",
    tools: [vfsReadFile("/kb/webhook-delivery.md", KB_WEBHOOKS)],
    text: "Here is how delivery verification works, from the KB:\n\n1. **Read the raw request bytes.** Do not parse and re-serialize the JSON — key order and whitespace change and the digest will not match. This is nearly every \"signature never matches\" case.\n2. **Check the timestamp.** Reject if `X-Northwind-Timestamp` is more than 300 seconds from now, either direction.\n3. **Compute the HMAC** over `\"{timestamp}.{raw_body}\"` with the endpoint's `whsec_…` secret, hex-encoded, and compare it to the value after `v1=` in `X-Northwind-Signature` with a constant-time comparison.\n\n```js\nconst expected = createHmac(\"sha256\", secret).update(`${ts}.${rawBody}`).digest(\"hex\");\nconst given = headers[\"x-northwind-signature\"].replace(/^v1=/, \"\");\nreturn timingSafeEqual(Buffer.from(expected), Buffer.from(given));\n```\n\nRetries: a non-2xx or a 10 s timeout is retried at 1 min, 5 min, 30 min, 2 h, 8 h, 24 h, always with the same `X-Northwind-Delivery-Id` — store it and skip repeats.\n\nWhich framework are you on? Express, Next.js, and Rails each need the raw body captured explicitly, and I can give you the exact line.",
  };
}

function rateLimitTurn(turn: number): DemoTurn {
  if (turn > 1) {
    return {
      text: "Quick version: limits are per key and per environment, `429` comes with `Retry-After` (sleep at least that long, add jitter), and `RateLimit-Remaining` on every response lets you slow down before you hit it. If several workers share one key they share one bucket — a key per service is the usual fix.\n\nSend me the request id of one 429 (`req_…`) and the key prefix, and I will tell you which window it tripped. Limit increases go through Accounts, not me — I can open that ticket.",
    };
  }
  return {
    thinking: "Rate limits. Quote the plan numbers from the KB rather than from memory, and check whether this is the polling pattern.",
    tools: [vfsReadFile("/kb/rate-limits.md", KB_RATE_LIMITS)],
    text: "Rate limits, from the KB:\n\n| Plan | Sustained | Burst |\n| --- | --- | --- |\n| Developer | 60 req/min | 100 in 10 s |\n| Team | 600 req/min | 1,000 in 10 s |\n| Business | 3,000 req/min | 5,000 in 10 s |\n\nBoth counters are **per key and per environment**; sandbox is one tenth of these. Every response carries `RateLimit-Limit`, `RateLimit-Remaining`, and `RateLimit-Reset`, and a `429` carries `Retry-After` — sleep at least that long, back off exponentially if it repeats, and add jitter so workers do not collide on the same second.\n\nThe usual causes: polling `/v3/events` instead of using webhooks (the list endpoint has its own lower limit on the Developer plan), and many workers on one key. If you send me the request id of one 429 and the first 12 characters of the key, I will check which window it hit. A limit increase is an Accounts decision, not mine — I can open that ticket and say who has it.",
  };
}

function authTurn(turn: number): DemoTurn {
  if (turn > 1) {
    return {
      text: "The short rule: a **401** is the credential itself (unknown, revoked, expired, or a sandbox key on production — `environment_mismatch`), a **403** is what it may do (`insufficient_scope` names the missing scope; `key_restricted` is an IP allowlist). Never rotate a key for a 403.\n\nGive me the request id (`req_…`) of one failing call and I will read the exact reason from the log — I only need the first 12 characters of the key, never the whole thing.",
    };
  }
  return {
    thinking: "An authentication question. The 401-vs-403 table is in the KB; read it and ask for a request id rather than guessing at the reason.",
    tools: [vfsReadFile("/kb/api-authentication.md", KB_AUTH)],
    text: "Here is how to read an auth failure, from the KB:\n\n| Status | `error.code` | Meaning |\n| --- | --- | --- |\n| 401 | `invalid_credentials` | Key or token unknown, revoked, or malformed — check for `Bearer <token>` with one space and no trailing newline |\n| 401 | `token_expired` | OAuth access token past its hour — refresh |\n| 401 | `environment_mismatch` | Sandbox key (`nw_test_…`) on production or the reverse |\n| 403 | `insufficient_scope` | Valid credential, missing scope — the response names `required_scope` |\n| 403 | `key_restricted` | The key has an IP allowlist and you are outside it |\n\nA 401 is always the credential; a 403 is always what it may do — so a new key never fixes a 403. Key rotation is overlap-safe: create the second key, deploy it, then revoke the old one (revocation is immediate).\n\nSend me the request id (`X-Request-Id`, `req_…`) of one failing call and the first 12 characters of the key, and I will read the exact reason from the log. Plan questions (\"does my plan include OAuth apps?\") go to Accounts — I can open that ticket.",
  };
}

function migrationTurn(turn: number): DemoTurn {
  if (turn > 1) {
    return {
      text: "In short: run `npx @northwind/codemod v2-to-v3 ./src` first; it rewrites `events.on` → `events.subscribe()`, offset pages → cursors, `apiKey` → `auth: { apiKey }`, and typed errors, and leaves a `TODO(northwind-v3)` wherever a listener had side effects it could not move. v2 gets security fixes until 2026-12-31 and its long-polling endpoint goes away 2027-06-30.\n\nWhich SDK and version are you on (`npm ls @northwind/sdk` or `pip show northwind`), and which call broke? I will open a ticket tagged `sdk-v3` so it is counted.",
    };
  }
  return {
    thinking: "SDK v3. The breaking-change table and the codemod are in the KB; the events.on removal is the usual one.",
    tools: [vfsReadFile("/kb/sdk-v3-migration.md", KB_SDK)],
    text: "The v3 changes that break code, from the KB:\n\n| v2 | v3 |\n| --- | --- |\n| `client.events.on(\"delivery\", fn)` | `for await (const event of client.events.subscribe({ types: [\"delivery.completed\"] }))` |\n| `client.events.list({ page: 3 })` | `client.events.list({ after: cursor, limit: 50 })` |\n| `client.webhooks.verify(body, sig)` | `client.webhooks.verify({ rawBody, headers })` |\n| `{ status, message }` errors | `NorthwindError` with `.code`, `.requestId`, `.retryAfter` |\n| `apiKey` option | `auth: { apiKey }` or `auth: { accessToken, refresh }` |\n\nRun the codemod first — `npx @northwind/codemod v2-to-v3 ./src` (Node) or `northwind-codemod v2-to-v3 ./app` (Python). It rewrites everything static and leaves a `TODO(northwind-v3)` plus a `subscribe` skeleton where a listener had side effects inside the callback; offset-pagination loops are flagged for you to rewrite as a `for await`.\n\nTimeline: v2 gets security fixes until 2026-12-31; its long-polling endpoint is removed 2027-06-30 (`410 Gone`). Tell me which SDK and version you are on and which call broke, and I will open a ticket tagged `sdk-v3`.",
  };
}

function bugTurn(turn: number): DemoTurn {
  if (turn > 1) {
    return {
      text: "With two request ids, the exact input, and the key type I can reproduce it with the `nw` CLI in the support-tools environment. If it reproduces and the documented behavior differs, it becomes an escalation: I open a Zendesk ticket, `bot_emit` a `bug.confirmed` event to the escalations bot with the minimal repro, and it searches `northwind/platform` for a duplicate before filing an issue — I relay the issue number back to you from the receipt, and the ticket stays open until the fix ships.\n\nPaste the request ids when you have them.",
    };
  }
  return {
    thinking: "A bug report. The matrix says: reproduced, documented behavior differs, minimal repro — before anything goes to engineering. Read it, then ask for exactly what I need.",
    tools: [vfsReadFile("/kb/escalation-matrix.md", KB_ESCALATION)],
    text: "That may well be a bug — let me collect what engineering needs, so it goes over once:\n\n1. **Request ids** — the `X-Request-Id` (`req_…`) of at least two requests that show it, so it is not one bad node.\n2. **The exact input and output** — endpoint, parameters, the response body, and what you expected instead (a doc or reference link helps).\n3. **Key type and build** — sandbox or production, and the `X-Northwind-Build` response header. The sandbox runs next week's build, so a sandbox-only bug is a regression worth catching now.\n4. **Client** — curl, or the SDK and version.\n\nWith that I reproduce it in the sandbox, open a Zendesk ticket for you, and hand the repro to the escalations bot, which checks `northwind/platform` for an existing issue before filing a new one. I relay the issue number back here, and the ticket stays open until the fix ships — no dates promised.",
  };
}

function incidentTurn(turn: number): DemoTurn {
  if (turn > 1) {
    return {
      tools: [webFetch(STATUS_FEED_URL, "status page", STATUS_FEED_NOW)],
      text: "Checked again: still one open incident — the sandbox API error rate in eu-1, severity 3, monitoring and back at baseline; production untouched. The Aug 19 webhook delay incident stays resolved.\n\nIf you have a request id or a delivery id from something that looks broken right now, paste it and I will compare it against the status page before raising anything with on-call.",
    };
  }
  return {
    thinking: "Anything about outages starts with the status page, not with memory. Fetch the feed and summarize what is open.",
    tools: [webFetch(STATUS_FEED_URL, "status page", STATUS_FEED_NOW)],
    text: `Here is the current picture from the status page:\n\n**Open: Elevated error rate on the sandbox API (eu-1)** — severity 3, monitoring. About 2% of sandbox requests in eu-1 returned 5xx; the affected nodes were rotated out and error rates are back to baseline. Production was never affected. No developer action needed; a sandbox retry is enough.\n\n**Resolved Aug 19: Webhook delivery delays (EU)** — severity 2, 14:02–15:20. A delivery-worker build tripled CPU per delivery and the EU backlog drained after a rollback; nothing was dropped, but 212 deliveries went out twice with the same delivery id between 15:03 and 15:20. The postmortem is in \`/postmortems\` with four action items open.\n\nIf a developer asks: "It's known, sandbox only, already recovering." If you are seeing something the status page does not list, tell me what and where and I will raise it with on-call.`,
  };
}

function defaultTurn(input: string, turn: number): DemoTurn {
  const words = input.trim().split(/\s+/);
  const topic = words.slice(0, 8).join(" ") + (words.length > 8 ? "…" : "");
  if (turn > 1) {
    return {
      text: `Still with you. On "${topic}", the quickest path is:\n\n1. **Request id** — reply with one (\`req_…\`) and I will read the exact reason from the log.\n2. **Knowledge base** — I answer from \`/kb\` (authentication, webhook delivery, rate limits, SDK v3 migration, sandbox vs production), so ask the question the way the developer asked it.\n3. **Ticket** — if it needs a person or a follow-up, I open one, link it here, and say who has it.\n\nWhich one?`,
    };
  }
  return {
    text: "Happy to help with that. Here is what I would do next:\n\n1. **Get a request id** — every API response carries `X-Request-Id` (`req_…`); with one I can read the exact reason for an error from the log, and I only ever need the first 12 characters of a key.\n2. **Check the knowledge base** — the pages under `/kb` cover authentication (401 vs 403), webhook delivery and signatures, rate limits, the SDK v3 migration, and sandbox vs production; I answer from those first and quote their numbers.\n3. **Open a ticket if it needs a person** — plan and billing questions go to Accounts; reproduced bugs go to engineering with a minimal repro, and I relay the issue number back.\n\nWhich one should I start with?",
  };
}

const responder: DemoResponder = (input, context) => {
  const text = input.toLowerCase();
  if (/webhook|signature/.test(text)) return webhookTurn(context.turn);
  if (/\b429\b|rate.?limit|\brate\b/.test(text)) return rateLimitTurn(context.turn);
  if (/\b40[13]\b|\bauth|api key|\bkey\b|token/.test(text)) return authTurn(context.turn);
  if (/\bsdk\b|\bv3\b|migrat/.test(text)) return migrationTurn(context.turn);
  if (/\bbug|duplicate|escalat|repro/.test(text)) return bugTurn(context.turn);
  if (/incident|\bstatus\b|outage|\bdown\b/.test(text)) return incidentTurn(context.turn);
  return defaultTurn(input, context.turn);
};

// ---------------------------------------------------------------------------
// Seed
// ---------------------------------------------------------------------------

export function seedTechnicalSupport(store: DemoStore): void {
  const universe = store.addUniverse({
    id: TECHNICAL_SUPPORT_UNIVERSE_ID,
    slug: TECHNICAL_SUPPORT_SLUG,
    name: "Technical Support",
    lightspeedUniverseId: LIGHTSPEED_UNIVERSE_ID,
    role: "owner",
    createdAt: agoIso(49 * DAY_MS),
    responder,
  });
  seedMembers(store, universe);
  seedProfiles(universe);
  seedWorkspaces(store, universe);
  seedEnvironments(universe);
  seedIntegrations(universe);
  seedChannels(universe);
  seedHelpdesk(store, universe);
  seedEscalations(store, universe);
  seedStatusWatch(store, universe);
  seedOncall(store, universe);
  seedInteractiveSessions(store, universe);
}
