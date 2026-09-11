/// Personal Assistant: one founder's always-on assistant, reached from her
/// phone. Ada runs Lumen; her assistant bot wakes on a 30-minute heartbeat
/// (and mostly decides to stay silent), sends the morning brief and the
/// evening wrap, watches the inbox and the calendar, drafts mail for a
/// one-word approval, keeps its own memory in a workspace it reads and
/// writes, delegates research to a second bot that runs sub-agents, and
/// takes the Monday numbers from a metrics bot — the personal-agent pattern
/// built from bots, triggers, workspaces, skills, and one Mac mini at home.
import type { Environment, ProfileEnvironment, SecretGrant, UniverseSetup } from "@/api";
import type { SessionSummaryView } from "@lightspeed-ai/agent-client";
import { appendExchange, appendScriptedRun, closeSession, newSession } from "../engine";
import type { DemoResponder, DemoStore, DemoToolCall, DemoTurn, SessionRecord, UniverseState } from "../store";
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
  SELF_CONFIG_TOOLS,
  SONNET,
  ago,
  agentSpawn,
  agoIso,
  at,
  atIso,
  awaitPromises,
  bot,
  botEmit,
  botSession,
  botState,
  channelAccount,
  channelPairing,
  chatMessage,
  chatSent,
  chatTrigger,
  clockLabel,
  eventLog,
  hex,
  inboxTrigger,
  lineageChild,
  managedSession,
  mcpCall,
  mcpServer,
  member,
  messageSend,
  modelDiscovery,
  modelOption,
  modelProvider,
  pollTrigger,
  profile,
  providerBinding,
  receipt,
  recent,
  runCommand,
  scheduleTrigger,
  subagentSession,
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
import { INCUS_PROVIDER_ID, TELEGRAM_ADA_ACCOUNT_ID, WHATSAPP_ADA_ACCOUNT_ID } from "./platform";

export const PERSONAL_ASSISTANT_UNIVERSE_ID = "33333333-3333-4333-8333-333333333333";
export const PERSONAL_ASSISTANT_SLUG = "personal-assistant";
const LIGHTSPEED_UNIVERSE_ID = "0000c0c0-0000-4000-8000-0000000c0c0c";

const PROFILE = {
  assistant: "executive-assistant",
  researcher: "researcher",
  metrics: "metrics-analyst",
  writer: "briefing-writer",
} as const;
const WORKSPACE = { memory: "assistant-memory", skills: "assistant-skills", briefs: "briefs" } as const;
const ENV_MAC_MINI = "env-ada-mac-mini";
const MCP = { google: "google-workspace", slack: "slack", stripe: "stripe", hubspot: "hubspot", notion: "notion" } as const;
const GRANT = {
  google: "grant-google-workspace-oauth",
  slack: "grant-slack-bot-token",
  stripe: "grant-stripe-restricted-key",
  stripeSigning: "grant-stripe-webhook-secret",
  hubspot: "grant-hubspot-oauth",
  notion: "grant-notion-oauth",
  anthropic: "grant-anthropic-key",
} as const;
const BOT = { assistant: "assistant", research: "research", metrics: "metrics" } as const;
const TELEGRAM_SOURCE = "telegram:ada_assistant_bot";
const WHATSAPP_SOURCE = "whatsapp:+4917612345678";
const WORKSPACE_MCP_URL = "https://workspace.mcp.google.example";
const STRIPE_WEBHOOK_TOKEN = "c7e2a94f10b64d3e8f5a2c1b9d0e7f36";

/// The assistant's threads: the Telegram one is keyed `ada`, the same key
/// the schedule and inbox triggers route with, so the brief, the heartbeat,
/// the prep notes, and the research receipts all land where Ada reads them.
const SESSION = {
  assistantMain: `bot:v1:${BOT.assistant}`,
  telegram: `bot:v1:${BOT.assistant}:k-ada`,
  whatsapp: `bot:v1:${BOT.assistant}:k-whatsapp-dm-ada-demo`,
  researchMain: `bot:v1:${BOT.research}`,
  researchPricing: `bot:v1:${BOT.research}:e-${hex("research:pricing", 8)}`,
  researchNudge: `bot:v1:${BOT.research}:e-${hex("research:nudge", 8)}`,
  subBeacon: "session-sub-pricing-beacon",
  subOrbit: "session-sub-pricing-orbit",
  metricsMain: `bot:v1:${BOT.metrics}`,
  board: "session-pa-board-update",
  marco: "session-pa-prep-marco",
  hiring: "session-pa-hiring-pipeline",
} as const;

const TELEGRAM: Conversation = {
  sessionId: SESSION.telegram,
  label: "telegram dm · Ada Demo",
  provider: "telegram",
  source: TELEGRAM_SOURCE,
  chatId: "88213",
  scope: "direct",
};
const WHATSAPP: Conversation = {
  sessionId: SESSION.whatsapp,
  label: "whatsapp dm · Ada Demo",
  provider: "whatsapp",
  source: WHATSAPP_SOURCE,
  chatId: "4915209876543@s.whatsapp.net",
  scope: "direct",
};

/// Receiver-side #N of every cross-bot event, fixed by each log's order
/// below; the smoke check asserts they line up.
const SEQ = {
  researchRequest: 1,
  researchNudge: 2,
  assistantMetricsWeekly: 1,
  assistantBillingAlert: 3,
} as const;

// ---------------------------------------------------------------------------
// The assistant's day
// ---------------------------------------------------------------------------

function pad(value: number): string {
  return String(value).padStart(2, "0");
}

/// `YYYY-MM-DD` in local time, for file names, logs, and search queries.
function dateLabel(ms: number): string {
  const date = new Date(ms);
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

function weekdayName(ms: number): string {
  return new Date(ms).toLocaleDateString("en-US", { weekday: "long" });
}

/// ISO week number of the day containing `ms`.
function isoWeek(ms: number): number {
  const date = new Date(ms);
  const weekday = (date.getDay() + 6) % 7;
  const thursday = new Date(date.getFullYear(), date.getMonth(), date.getDate() - weekday + 3);
  const january4 = new Date(thursday.getFullYear(), 0, 4);
  const week1Monday = new Date(january4.getFullYear(), 0, 4 - ((january4.getDay() + 6) % 7));
  return 1 + Math.round((thursday.getTime() - week1Monday.getTime()) / (7 * DAY_MS));
}

/// Today's story runs 07:00 → 15:00. It is told at real clock times once
/// enough of the day has passed and compressed into what has passed
/// otherwise, so nothing lands in the future and the order always holds.
const STORY_MINUTES = 8 * 60;
const sevenToday = at(0, 7, 0);
const dayStart =
  sevenToday <= NOW - 30 * MINUTE_MS ? sevenToday : Math.max(NOW - (STORY_MINUTES + 10) * MINUTE_MS, at(1, 18, 0));
const dayScale = Math.min(MINUTE_MS, (NOW - 10 * MINUTE_MS - dayStart) / STORY_MINUTES);
/// A moment of today's story at `hh:mm`.
const t = (hh: number, mm: number): number => dayStart + ((hh - 7) * 60 + mm) * dayScale;
/// The clock label of a story moment, as the transcripts quote it.
const hm = (hh: number, mm: number): string => clockLabel(t(hh, mm));

const TODAY = dateLabel(NOW);
const YESTERDAY = dateLabel(ago(DAY_MS));
const TOMORROW = dateLabel(NOW + DAY_MS);
const daysAgoLabel = (days: number): string => dateLabel(ago(days * DAY_MS));
/// The Monday whose 07:30 digest has run, at least three days back so the
/// week's history sits before the last two days' story.
const MONDAY_DAYS_AGO = (() => {
  const days = (new Date(NOW).getDay() + 6) % 7 || 7;
  return days < 3 ? days + 7 : days;
})();
const MONDAY = daysAgoLabel(MONDAY_DAYS_AGO);
/// Days back to the last Friday whose 16:00 review has run.
const FRIDAY_DAYS_AGO = (new Date(NOW).getDay() + 2) % 7 || 7;
const REVIEW_AT = at(FRIDAY_DAYS_AGO, 16, 0);
const REVIEW_WEEK = isoWeek(REVIEW_AT);
const REVIEW_FILE = `week-${REVIEW_WEEK}-review.md`;

/// Today's calendar, in story time.
const CAL = {
  standup: t(9, 0),
  northlight: t(10, 30),
  priya: t(14, 0),
  debrief: t(16, 0),
} as const;

const main = (botId: string): { sessionId: string; label: string } => ({ sessionId: `bot:v1:${botId}`, label: "Main" });

// ---------------------------------------------------------------------------
// Tool calls the builders do not cover
// ---------------------------------------------------------------------------

/// A script on the Mac mini at home.
function mac(argv: string[], output: string, isError = false): DemoToolCall {
  return runCommand(argv, output, { detail: ENV_MAC_MINI, cwd: "/Users/ada/assistant", timeoutMs: 120_000, isError });
}

/// An MCP call the server refused.
function mcpFailure(name: string, args: Record<string, unknown>, output: string): DemoToolCall {
  return tool(name, name, args, mcpDisplay(name, args), output, true);
}

function closeAt(session: SessionRecord, atMs: number): void {
  closeSession(session, true, atMs);
}

// ---------------------------------------------------------------------------
// Memory workspace
// ---------------------------------------------------------------------------

const MEMORY_README = `# Ada's assistant — how this works

This workspace is the assistant's memory. It is mounted read-write at /memory in every session the \`assistant\` bot runs; the playbooks under /skills are mounted read-only; the briefs it writes go to /briefs. Everything the assistant knows between conversations is in these files, and nothing else.

## What wakes it
| Trigger | When | What it does |
| --- | --- | --- |
| \`telegram\`, \`whatsapp\` | a message from Ada's paired phone | replies in that thread with \`message_send\`; one thread per chat, kept forever |
| \`morning-brief\` | weekdays 07:00 | calendar, inbox highlights, prep needed, travel → the Telegram thread and /briefs |
| \`heartbeat\` | every 30 min, 08:00–22:00 weekdays | reads the checklist below and usually decides there is nothing worth saying |
| \`evening-wrap\` | weekdays 17:30 | what got done, what carried over, tomorrow's first meeting |
| \`friday-review\` | Fridays 16:00 | the week: commitments kept and slipped, people to follow up, next week's shape |
| \`inbox-poll\` | every 10 min | unread mail labelled IMPORTANT → Urgent / Needs reply / FYI / Archive, replies drafted |
| \`calendar-poll\` | every 15 min | a meeting starting within 40 minutes → a prep note in the thread |
| \`inbox\` | events from \`research\` and \`metrics\` | delegated research landing, the Monday numbers, billing alerts |

## What it may do on its own
- Read mail and calendar, search Slack, read and write these files.
- Message Ada (quiet hours 23:00–08:00 unless urgent).
- Create mail drafts, hold internal meetings, ask \`research\` for work, run scripts and the browser on the Mac mini at home (\`env-ada-mac-mini\`).

## What needs an explicit yes in the thread
- Sending any email. It drafts; Ada says "send it".
- Accepting, declining, or moving a meeting with anyone outside Lumen.
- Anything that spends money: bookings, subscriptions, tools.

## Where memory lives
- \`MEMORY.md\` — durable facts: Ada, the company, standing arrangements. Rewritten rarely.
- \`preferences.md\` — how Ada likes things done. Edited when she says so.
- \`commitments.md\` — what Ada promised, to whom, by when. Every promise made in a meeting or a message lands here the same day.
- \`people/\` — one file per person who matters this quarter, with the last interactions.
- \`daily/YYYY-MM-DD.md\` — the running log for the day; the evening wrap closes it, the morning brief opens the next one.
- \`research/\` — what the \`research\` bot delivers.

## Heartbeat checklist
1. Anything in today's log marked \`[ ]\` past its time?
2. Mail Ada sent with no reply after 48 h → suggest a nudge (draft only).
3. A meeting in the next 40 minutes without a prep note?
4. Research or metrics pending longer than expected?
If all four are no, say nothing.

## Changing behaviour
Tone, quiet hours, and the approval rules live in the \`executive-assistant\` profile's instructions; the schedule lives on the bot's triggers; this checklist lives here. Change the file, not the model.
`;

const MEMORY_MAIN = (mrr: string, churn: string, digest: string): string => `# Memory

Ada Demo — founder and CEO of Lumen, B2B workflow software, ~40 people, HQ Munich, team across DE/NL/UK. Series A in progress: Northlight Ventures (partner Elena Marsh) is the lead conversation; Kestrel Capital (partner Tom Adeyemi) a possible second. Data room opened Aug 18.

## Standing arrangements
- Leadership standup Mon–Thu 09:00, Marco runs it; Ada speaks last.
- 1:1 Priya (head of engineering) Thursdays 14:00; 1:1 Marco (chief of staff) Mondays 11:00.
- Board: quarterly. Q3 update due Sep 12; Ada's metrics section to Marco by Sep 5.
- Sep 15–16: B2B SaaS Summit, Berlin, at Alte Münze (Molkenmarkt 2). Train from Munich, one night, hotel booked ${daysAgoLabel(2)} (Hotel Luc, confirmation LUC-48213).

## Company facts
- MRR ${mrr} (metrics digest ${digest}); net revenue churn ${churn}; 31 paying customers, largest Acme Corp (€38.4k/mo).
- Hiring: Staff engineer (Priya's), Account executive (Marco's). Offer out to one Staff candidate; decision by Sep 3.
- Runway 19 months at current burn (finance memo Aug 20).

## Assistant setup
- Reached on Telegram and WhatsApp from Ada's phone only; the Telegram thread is where briefs and prep notes go.
- Mac mini at home runs scripts and a headless browser for anything without an API (hotels, trains, the summit site).
`;

const MEMORY_BEFORE_DIGEST = MEMORY_MAIN("€398k", "1.4%", daysAgoLabel(MONDAY_DAYS_AGO + 7));
const MEMORY_CURRENT = MEMORY_MAIN("€412k", "1.1%", MONDAY);

const PREFERENCES = `# Preferences

- Tone: short, plain, no exclamation marks. Numbers over adjectives. Lead with the decision needed.
- On the phone: six lines at most unless asked for detail; bullets, no headers, no emoji.
- Quiet hours: 23:00–08:00 Europe/Berlin. Urgent means a customer outage, a signed term sheet, or family. Everything else waits for the morning brief.
- Mail: draft in Ada's voice — first name, one paragraph, one ask, sign "Ada". Never send unasked; say what will be sent and wait for "send it".
- Calendar: hold internal meetings freely. Anything with an investor, a customer, or a candidate needs a yes. Default lengths 25/50 minutes. Nothing before 09:00 or after 18:00 unless Ada set it.
- Meeting prep: attendees, last interaction, open threads, one suggested opening question. About 40 minutes before.
- Travel: train under 5 h, otherwise economy. Hotel within walking distance of the venue, under €220/night without asking. Two options, never more; Ada picks.
- Research: cite sources; a table beats prose; say what could not be found.
- Citations: when a claim comes from memory, name the file (people/marco.md, commitments.md).
`;

const COMMITMENTS_MORNING = `# Commitments

| Made | To | What | By | Status |
| --- | --- | --- | --- | --- |
| ${MONDAY} | Priya | Decide on the Staff engineer offer (band, start date) | Sep 3 | open — comp sheet arrived ${YESTERDAY} (people/priya.md) |
| ${MONDAY} | Priya | Review the on-call rota change before it goes to the team | ${YESTERDAY} | done ${YESTERDAY} 16:10, Slack #eng-leads |
| ${daysAgoLabel(2)} | Marco | Q3 board update: metrics section | Sep 5 | open — draft started (session "Board update — Q3 draft") |
| ${daysAgoLabel(3)} | Elena Marsh (Northlight) | Cohort retention cut and three customer references | ${TODAY} | open — cut is in the data room; references cleared (people/investors.md) |
| ${YESTERDAY} | Priya | Introduce her to Kestrel's CTO-in-residence | Sep 5 | open — depends on Tom replying |
| ${YESTERDAY} | Marco | Finance to model Acme's 2-year term with a price hold | Sep 2 | open — Marco has it |
| ${daysAgoLabel(3)} | Tom Adeyemi (Kestrel) | Follow-up call after his data-room review | week of Sep 7 | waiting — no reply for 48 h; nudge drafted ${TODAY}, not sent |
`;

const COMMITMENTS_AFTER_SEND = COMMITMENTS_MORNING.replace(
  `| ${TODAY} | open — cut is in the data room; references cleared (people/investors.md) |`,
  `| ${TODAY} | done ${TODAY} ${hm(11, 25)} — sent with the cut attached and three references (Gmail) |`,
);

const PEOPLE_MARCO = `# Marco Ruiz — chief of staff

Runs the leadership standup, owns the board pack and the AE hire. Wants a written note before a meeting; hates surprises in standup. Reads Slack, not email.

## Open threads
- Q3 board update: he has the narrative; needs Ada's metrics section by Sep 5 (commitments.md). New deck template coming from him.
- AE hiring: two finalists, panel next week; wants Ada on the final interview.
- Acme Corp renewal: modelling the 2-year term with a price hold, due Sep 2 (commitments.md).
- Offsite Oct 8–9: venue shortlist lives in Notion — the Notion connection needs re-auth, so read it from his Slack summary.

## Last interactions
- ${YESTERDAY} standup: the board deck template changed; he sends the new one this week.
- ${daysAgoLabel(2)} 1:1: agreed the metrics section is Ada's; he writes hiring and product.
- ${MONDAY} 11:00 1:1: asked Ada to stop moving Priya's 1:1 ("third time this month").
`;

const PEOPLE_PRIYA = `# Priya Natarajan — head of engineering

Joined 2025-03; runs 14 engineers in three squads. Direct; wants decisions with reasons, in writing. Weekly 1:1 Thursdays 14:00.

## Open threads
- Staff engineer offer: candidate Jonas Weber; comp sheet sent ${YESTERDAY}; decision due Sep 3, the candidate's other deadline is Sep 4 (commitments.md).
- On-call rota change: approved by Ada ${YESTERDAY} 16:10 in #eng-leads; goes to the team Monday.
- Wants an intro to Kestrel's CTO-in-residence for the platform migration plan (commitments.md).
- Platform migration: Q4 plan draft due mid-September; asked Ada not to commit dates to the board before it is costed.

## Last interactions
- ${YESTERDAY} 1:1: comp sheet; rota; migration costing not done yet.
- ${MONDAY} standup: asked for the offer decision before Sep 4.
- ${daysAgoLabel(8)} 1:1: migration plan risks; "no dates to the board yet".
`;

const PEOPLE_INVESTORS = `# Investors — Series A

## Northlight Ventures
- Partner: Elena Marsh (lead). Associate: Sam Okoro (runs the data-room questions).
- Thesis fit: B2B workflow software with usage-based expansion. Cares about net retention and the second product.
- Asks so far: cohort retention cut by month for the 2025 cohorts; three customer references; Q3 pipeline coverage (Marco preparing).
- References cleared by Ada: Dana Whitfield (CTO, Acme Corp), Ines Brandt (COO, Halden Logistics), Kofi Mensah (VP Ops, Marrow Health). All warned, all fine with a call this week.
- Style: brisk, reads on the phone; keep mails to five lines. Says what she means: "term sheet discussion after references" (${daysAgoLabel(9)}).

### Last interactions
- ${daysAgoLabel(3)} email: Sam asked for the cohort cut "before Thursday"; Ada promised it — commitments.md.
- ${daysAgoLabel(9)} partner meeting: positive; Elena wants to see the second product's usage before IC.

## Kestrel Capital
- Partner: Tom Adeyemi. Second-close candidate, not lead. Has a CTO-in-residence (Lena Fischer) Priya wants to meet.
- Asks: data-room review (done Aug 22, "a few questions on gross margin"); follow-up call — Ada proposed the week of Sep 7 on ${daysAgoLabel(3)}, no reply since.
- Style: slow to reply, decisive once he does. Do not nudge twice.
`;

const PEOPLE_INVESTORS_AFTER_SYNC = PEOPLE_INVESTORS.replace(
  "### Last interactions\n",
  `### Last interactions\n- ${TODAY} ${clockLabel(CAL.northlight)} sync: Elena wants the cohort cut and the references before IC on Sep 8; sent ${hm(11, 25)}.\n`,
);

const DAILY_YESTERDAY = `# ${YESTERDAY} (${weekdayName(ago(DAY_MS))})

- 07:00 brief sent. 4 meetings, 2 important mails, no travel.
- 09:00 standup — Marco: new board deck template coming. [x]
- 11:20 mail.important: Sam Okoro (Northlight) on the data room — Ada answered it herself. [x]
- 14:00 1:1 Priya — comp sheet received; rota approved 16:10 in #eng-leads. [x]
- 15:20 prep sent: Acme Corp renewal call. [x]
- 16:00 Acme Corp renewal — Ada: "they want a 2-year term with a price hold; finance to model" → commitments.md (Marco, Sep 2). [x]
- 17:30 wrap sent. Carried over: Tom Adeyemi (Kestrel) has not replied for 48 h → draft a nudge in the morning, do not send.
- Heartbeats: 18 silent, 1 spoke (15:20 prep).
`;

const DAILY_TODAY_OPEN = `# ${TODAY} (${weekdayName(NOW)})

- ${hm(7, 0)} brief sent. 4 meetings; 1 reply waiting on Ada (Kestrel nudge drafted, not sent); no travel.
- [ ] ${clockLabel(CAL.northlight)} Investor sync — Northlight Ventures: prep due ${hm(9, 50)}.
- [ ] ${clockLabel(CAL.priya)} 1:1 Priya.
- [ ] ${clockLabel(CAL.debrief)} Candidate debrief — Staff engineer: prep due ${hm(15, 20)}.
- [ ] Staff offer decision by Sep 3 (commitments.md).
`;

const DAILY_TODAY_MIDDAY = `${DAILY_TODAY_OPEN.replace(`- [ ] ${clockLabel(CAL.northlight)} Investor sync — Northlight Ventures: prep due ${hm(9, 50)}.\n`, `- [x] ${hm(9, 51)} prep sent: Investor sync — Northlight Ventures ${clockLabel(CAL.northlight)} (attendees, open asks, one question).\n`).replace(`- [ ] ${clockLabel(CAL.priya)} 1:1 Priya.\n`, `- [x] ${hm(10, 8)} Ada: 1:1 Priya moved ${clockLabel(CAL.priya)} → ${TOMORROW} same slot (calendar.update_event, invite updated). Marco's ${MONDAY} note: that is four this month.\n- [ ] ${hm(10, 8)} Ada: competitor pricing for the Series A deck by lunch → research #${SEQ.researchRequest}.\n`)}`;

const DAILY_TODAY = `${DAILY_TODAY_MIDDAY.replace(`- [ ] ${hm(10, 8)} Ada: competitor pricing for the Series A deck by lunch → research #${SEQ.researchRequest}.\n`, `- [x] ${hm(10, 8)} Ada: competitor pricing for the Series A deck by lunch → research #${SEQ.researchRequest}; nudged ${hm(11, 0)}; landed ${hm(11, 9)} → research/competitor-pricing.md.\n- [x] ${hm(11, 17)} mail.important: Elena Marsh — post-sync follow-ups (cohort cut, references). Drafted → Ada "send it" ${hm(11, 24)} → sent ${hm(11, 25)}. commitments.md updated.\n- [x] ${hm(13, 40)} Ada: "what did I promise Priya this week" → answered from commitments.md.\n`)}- Heartbeats so far: ${hm(8, 30)} silent, ${hm(11, 0)} spoke (nudged research), ${hm(14, 30)} silent.
`;

const RESEARCH_PRICING = `# Competitor pricing — Series A deck

Requested by the assistant ${TODAY} ${hm(10, 8)}; two sub-agents, one per competitor; public pages only, fetched ${TODAY}.

| | Lumen | Beacon Analytics | Orbit Insights |
| --- | --- | --- | --- |
| Entry tier | Team €49/user/mo | Starter $39/user/mo (min 5) | Growth $45/user/mo |
| Mid tier | Business €89/user/mo | Professional $79/user/mo | Scale $95/user/mo |
| Enterprise | custom, from €60k/yr | custom, "from $75k/yr" (pricing FAQ) | custom, no floor published |
| Usage component | workflow runs above 50k/mo, €0.004/run | events above 1M/mo, $0.50/1k | none — seats only |
| Annual discount | 15% | 20% | 10% ("billed annually") |
| Free tier | 14-day trial | free forever, 3 users | 30-day trial |
| Last price change | — | Mar 2026: Starter $29 → $39 (changelog) | Jan 2026: Scale introduced |

## Reading
- Lumen's mid tier is the most expensive per seat but the only one that scales with usage; the deck's "expansion from usage" claim stands.
- Beacon's free-forever tier is the funnel to beat at the low end; Orbit competes on seat price alone.
- Neither publishes net retention; Beacon's changelog implies a price rise stuck (no rollback since March).

## Sources
- beacon-analytics.example/pricing, /pricing/faq, /changelog (Mar 12, 2026 entry) — sub-agent "Pricing: Beacon Analytics".
- orbit-insights.example/pricing, /blog/introducing-scale — sub-agent "Pricing: Orbit Insights".
- lumen.example/pricing — for the baseline column.

## Could not find
- Orbit's enterprise floor and any usage pricing (none on the site; the sales page says "talk to us").
- Beacon's enterprise minimum seat count.
`;

const MEMORY_FILES: Record<string, string> = {
  "README.md": MEMORY_README,
  "MEMORY.md": MEMORY_CURRENT,
  "preferences.md": PREFERENCES,
  "commitments.md": COMMITMENTS_AFTER_SEND,
  "people/marco.md": PEOPLE_MARCO,
  "people/priya.md": PEOPLE_PRIYA,
  "people/investors.md": PEOPLE_INVESTORS_AFTER_SYNC,
  [`daily/${YESTERDAY}.md`]: DAILY_YESTERDAY,
  [`daily/${TODAY}.md`]: DAILY_TODAY,
  "research/competitor-pricing.md": RESEARCH_PRICING,
};

// ---------------------------------------------------------------------------
// Skills workspace
// ---------------------------------------------------------------------------

const SKILL_EMAIL_TRIAGE = `---
name: email-triage
description: Sort unread important mail into Urgent / Needs reply / FYI / Archive and draft the replies Ada would send, without sending them.
---

# Email triage

Runs on every \`inbox-poll\` event and whenever Ada asks about her inbox.

## Buckets
| Bucket | Test | Action |
| --- | --- | --- |
| Urgent | a customer outage, a signed document, a deadline today, family | message Ada now, even in quiet hours |
| Needs reply | a question or an ask addressed to Ada from someone in people/ or a customer | draft the reply with \`gmail.create_draft\`; tell Ada in one line and wait for "send it" |
| FYI | Ada is cc'd, or the mail is a status update with no ask | one line in the next brief; \`gmail.modify_labels\` adds \`assistant/fyi\` |
| Archive | newsletters, receipts, automated notifications already in Slack | \`gmail.modify_labels\` removes INBOX; count it in the brief |

## Drafting a reply
1. Read the whole thread with \`gmail.get_message\`, not the snippet.
2. Read the person's file under /memory/people if there is one; check /memory/commitments.md for anything already promised.
3. Draft in Ada's voice (preferences.md): first name, one paragraph, one ask, "Ada". Attach what was promised if it exists in Drive; never promise a new date.
4. Tell Ada: who, what they want, what the draft says in one sentence, and "say *send it* and I'll send it". Never send on your own.

## After "send it"
\`gmail.send\` the draft, mark the matching commitment done with the time, note it in today's log.
`;

const SKILL_MEETING_PREP = `---
name: meeting-prep
description: A one-screen prep note about 40 minutes before a meeting — attendees, last interactions, open threads, one opening question.
---

# Meeting prep

Runs on every \`calendar-poll\` event and whenever Ada asks to prep a meeting.

## Skip when
- The meeting is a standup, a 1:1 Ada runs weekly, or blocked focus time — unless Ada asked.
- A prep note for this event is already in today's log.

## Build the note
1. Attendees: names and roles. Internal people from /memory/people; external ones from the last thread with them (\`gmail.search from:<domain> newer_than:30d\`).
2. Last interaction: the most recent line under "Last interactions" in their file, or the last mail. Date it.
3. Open threads: every row in /memory/commitments.md naming them, with status; anything they asked for that is still open.
4. One opening question Ada could use — specific to the thread, never "how are things".
5. If a document is expected (a deck, a cut, a memo): where it is and whether it is done.

## Send it
Six lines on the phone, \`message_send\` in the Telegram thread. Then mark the calendar event prepped (\`calendar.update_event\` with the private extended property \`prepped=true\`) so the poll does not deliver it again, and add a \`[x]\` line to today's log.
`;

const SKILL_TRAVEL = `---
name: travel
description: Two bookable options for a trip — transport and a hotel near the venue — under the limits in preferences.md; book only the one Ada picks.
---

# Travel

Runs when Ada asks to book or plan a trip.

## Rules (preferences.md)
- Train under 5 h, otherwise economy. Arrive the evening before if the first session is before 10:00.
- Hotel within walking distance of the venue; under €220/night without asking; breakfast included when the difference is under €15.
- Two options, never more. Ada picks; nothing is booked before she does.

## Find options
1. The venue and dates come from /memory/MEMORY.md or the ask. Confirm the venue address.
2. Trains: \`trains.py <from> <to> <date> --arrive-by <hh:mm>\` on the Mac mini (there is no API; the script drives the booking site).
3. Hotels: \`web_fetch\` the aggregator's search page for the address and dates, then \`hotels.py --near <address> --checkin <date> --nights <n> --max <eur>\` to check live rates on the two shortlisted ones.
4. Present A and B: train times, hotel, walk to the venue, total. One line each.

## Book
1. \`book.py --option <A|B>\` on the Mac mini; it returns the confirmation numbers.
2. \`calendar.create_event\` for the train and for the hotel (check-in to check-out), confirmation numbers in the description.
3. Write the trip line into /memory/MEMORY.md under standing arrangements; tell Ada the total and the confirmation numbers.
`;

const SKILL_WEEKLY_REVIEW = `---
name: weekly-review
description: The Friday 16:00 review — commitments kept and slipped, people to follow up, the shape of next week — written to /briefs and sent as six lines.
---

# Weekly review

Runs on the \`friday-review\` schedule and whenever Ada asks for "the week".

## Sources, in this order
1. /memory/commitments.md — every row made or due this week.
2. /memory/daily/ — the five logs; count meetings, prep notes sent, heartbeats that spoke.
3. /memory/people/ — anyone whose last interaction is older than 14 days and has an open thread.
4. \`gmail.search in:sent newer_than:7d\` — mail Ada sent with no reply after 48 h.
5. The Monday metrics digest from /memory/MEMORY.md — numbers, no commentary.

## Write /briefs/week-<n>-review.md
- Kept / slipped / moved: commitments as a table with the reason for each slip, in Ada's words where the log has them.
- People: three names at most, each with the one thing to do.
- Next week: the meetings that need prep, travel, deadlines.
- Numbers: MRR, churn, pipeline, one line.

## Send
Six lines on the phone: kept/slipped counts, the one slip that matters, three names, next week's first hard deadline, and where the file is. Never a paragraph on a Friday afternoon.
`;

const SKILL_FILES: Record<string, string> = {
  "email-triage/SKILL.md": SKILL_EMAIL_TRIAGE,
  "meeting-prep/SKILL.md": SKILL_MEETING_PREP,
  "travel/SKILL.md": SKILL_TRAVEL,
  "weekly-review/SKILL.md": SKILL_WEEKLY_REVIEW,
};

// ---------------------------------------------------------------------------
// Briefs workspace
// ---------------------------------------------------------------------------

const BRIEF_MORNING = `# Morning brief — ${TODAY} (${weekdayName(NOW)})

## Calendar
- ${clockLabel(CAL.standup)} Leadership standup (30 min) — Marco runs it; no prep.
- ${clockLabel(CAL.northlight)} Investor sync — Northlight Ventures (45 min): Elena Marsh, Sam Okoro. **Prep at ${hm(9, 50)}.** Open asks: cohort cut (in the data room), three references (cleared).
- ${clockLabel(CAL.priya)} 1:1 Priya (50 min) — comp sheet arrived yesterday; she wants the offer decision before Sep 4.
- ${clockLabel(CAL.debrief)} Candidate debrief — Staff engineer (25 min): Priya, Marco. Prep at ${hm(15, 20)}.

## Inbox
- 11 unread since 17:30, 1 important: Sam Okoro (Northlight) confirming today's agenda — FYI, no reply needed.
- **Needs you:** Tom Adeyemi (Kestrel) has not replied to your ${daysAgoLabel(3)} mail for 48 h. A one-line nudge is drafted ("still keen on the week of Sep 7?"); it is not sent. Say *send it* or leave it — people/investors.md says do not nudge twice.
- 6 archived (2 newsletters, 3 receipts, 1 Stripe notification already in Slack).

## Commitments due
- Elena (Northlight): cohort cut and references, **today** — the cut is in the data room; references are in people/investors.md.
- Staff offer decision by Sep 3 — Priya will ask at ${clockLabel(CAL.priya)}.

## Travel
None today. Berlin, Sep 15–16: train and Hotel Luc booked (LUC-48213).
`;

const BRIEF_EVENING = `# Evening wrap — ${YESTERDAY} (${weekdayName(ago(DAY_MS))})

## Done
- Standup; 1:1 Priya (comp sheet received, rota approved 16:10); Acme Corp renewal call.
- Prep sent for the Acme call; 2 important mails handled (1 by Ada herself).

## Carried over
- Acme wants a 2-year term with a price hold → Marco models it by Sep 2 (added to commitments.md).
- Tom Adeyemi (Kestrel): 48 h without a reply → a nudge is drafted for the morning brief, not sent.
- Staff engineer offer decision (Sep 3): the comp sheet is in the 1:1 notes.

## Tomorrow
- First meeting ${clockLabel(CAL.standup)} standup; the one that needs prep is the Northlight sync at ${clockLabel(CAL.northlight)} — prep arrives ~40 minutes before.
- Heartbeats today: 18 silent, 1 spoke.
`;

const BRIEF_WEEK_REVIEW = `# Week ${REVIEW_WEEK} review — Friday ${dateLabel(REVIEW_AT)}

## Commitments
| Kept | Slipped | Moved |
| --- | --- | --- |
| 9 | 2 | 1 |

- **Slipped:** the Q3 pipeline coverage sheet for Northlight (Marco, "the CRM export was wrong twice"); the migration costing (Priya, waiting on vendor quotes).
- **Moved:** Priya's 1:1 to Friday — the third move this month; Marco noticed.

## People
- Tom Adeyemi (Kestrel): data-room review done, no follow-up call booked yet — propose the week of Sep 7 on Monday.
- Dana Whitfield (Acme): renewal call next week; she asked for a 2-year term in June and nobody answered in writing.
- Jonas Weber (Staff candidate): offer decision due Sep 3; his other deadline is Sep 4.

## Next week
- Mon 07:30 metrics digest; Mon 11:00 Marco; Thu ${clockLabel(CAL.northlight)} Northlight sync (prep needed); Thu 14:00 Priya; Fri 16:00 this review.
- No travel. Berlin summit Sep 15–16 still unbooked — book it.

## Numbers (from the ${daysAgoLabel(MONDAY_DAYS_AGO + 7)} digest)
MRR €398k, net revenue churn 1.4%, pipeline coverage 2.9× for Q4.
`;

const BRIEF_FILES: Record<string, string> = {
  [`${TODAY}-morning.md`]: BRIEF_MORNING,
  [`${YESTERDAY}-evening.md`]: BRIEF_EVENING,
  [REVIEW_FILE]: BRIEF_WEEK_REVIEW,
};

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

const ASSISTANT_INSTRUCTIONS = `You are Ada Demo's assistant. Ada is the founder and CEO of Lumen; you work for her alone, and you reach her on her phone.

Tone: short, plain, no exclamation marks, numbers over adjectives; lead with the decision she needs to make. Six lines on the phone unless she asks for more.

Quiet hours are 23:00–08:00 Europe/Berlin: no messages unless it is urgent — a customer outage, a signed document, family. Everything else waits for the morning brief.

You never send an email, never accept, decline, or move a meeting with anyone outside Lumen, and never spend money without an explicit yes from Ada in the thread. You draft, you hold, you propose two options; she says "send it", "yes", or picks A or B. Internal meetings you may move on your own — say so when you do.

Your memory is the workspace at /memory and nothing else. Read /memory/README.md once per session; read the person's file before a meeting or a reply; when a claim comes from memory, name the file it came from (people/priya.md, commitments.md). Keep /memory/daily/<today>.md and /memory/commitments.md current: every promise Ada makes in a meeting or a message lands in commitments.md the same day, and every prep note, draft, send, and decision gets a line in the daily log.

Playbooks are under /skills; follow email-triage, meeting-prep, travel, and weekly-review when the situation matches rather than improvising.

On a heartbeat, run the checklist in /memory/README.md and say nothing unless one item is yes. Most heartbeats are silent; that is correct.

Delegate research to the research bot (bot_emit, kind research.request, reply requested) and read its file when the receipt arrives. Use the Mac mini for anything without an API — the browser, trains, hotels — and say what you ran.`;

const RESEARCHER_INSTRUCTIONS = `You research one question at a time for Ada's assistant and answer with sources.

Split a question with independent parts across sub-agents (one per competitor, one per market) and join their results; do the synthesis yourself. Public pages only; fetch the page, quote the number, name the URL and the date. A table beats prose. Say plainly what you could not find rather than estimating.

Write the result to the path the request names under /memory/research, in the shape the request asks for, and finish with a two-line summary — the receipt carries it back.`;

const METRICS_INSTRUCTIONS = `You produce Lumen's Monday numbers and watch billing.

Every Monday 07:30: MRR, net revenue churn, new and churned customers, and pipeline coverage for the next quarter, from Stripe and HubSpot — the same six numbers every week, in the same order, with last week beside them. Emit the digest to the assistant as metrics.weekly; do not editorialise.

On a Stripe webhook: a failed payment or a cancelled subscription matters when it is a paying customer above €1k/mo or a third attempt; flag it to the assistant as billing.alert with the customer, the amount, and what Stripe does next. Trials and first attempts are noise — say so and stop.`;

const WRITER_INSTRUCTIONS = `You turn Ada's memory and briefs into documents other people read: board sections, investor updates, the Friday review when it needs more than six lines.

Read /memory/commitments.md and the relevant /briefs first; every number traces to a digest or a log line, and you name it in a footnote. Plain sentences, no adjectives, no claims memory does not support. When something is missing, leave a bracketed question for Ada rather than filling it in.`;

const link = (workspaceId: string, path: string, access: "readOnly" | "readWrite") => ({
  path,
  access,
  target: { type: "workspace", workspaceId },
});
const MEMORY_RW = link(WORKSPACE.memory, "/memory", "readWrite");
const MEMORY_RO = link(WORKSPACE.memory, "/memory", "readOnly");
const SKILLS_RO = link(WORKSPACE.skills, "/skills", "readOnly");
const BRIEFS_RW = link(WORKSPACE.briefs, "/briefs", "readWrite");
const BRIEFS_RO = link(WORKSPACE.briefs, "/briefs", "readOnly");

const ASSISTANT_LIMITS = { maxDepth: 1, maxDescendants: 4, maxConcurrent: 2, deadlineMs: 15 * MINUTE_MS };
const RESEARCH_LIMITS = { maxDepth: 1, maxDescendants: 6, maxConcurrent: 3, deadlineMs: 10 * MINUTE_MS };

const GOOGLE_TOOLS = [
  "gmail.search",
  "gmail.get_message",
  "gmail.create_draft",
  "gmail.send",
  "gmail.modify_labels",
  "calendar.list_events",
  "calendar.free_busy",
  "calendar.create_event",
  "calendar.update_event",
];
const SLACK_TOOLS = ["search_messages", "get_channel_history", "get_user"];

const ASSISTANT_CONFIG: Record<string, unknown> = {
  model: OPUS,
  generation: { reasoningEffort: "medium", maxOutputTokens: 8_000 },
  limits: { maxToolRounds: 16 },
  features: {
    vfs: { tools: "edit", workspaceLinks: [MEMORY_RW, SKILLS_RO, BRIEFS_RW], skills: { roots: ["/skills"] } },
    mcp: {
      servers: [
        { serverId: MCP.google },
        { serverId: MCP.slack },
      ],
    },
    environments: { selectionTools: false },
    subagents: { agents: [{ profileId: PROFILE.researcher }], ...ASSISTANT_LIMITS },
    web: { fetch: {} },
  },
};
const ASSISTANT_ENVIRONMENT: ProfileEnvironment = { type: "existing", environmentId: ENV_MAC_MINI };

const RESEARCHER_CONFIG: Record<string, unknown> = {
  model: SONNET,
  generation: { reasoningEffort: "medium", maxOutputTokens: 12_000 },
  limits: { maxToolRounds: 30 },
  features: {
    web: { fetch: {}, search: { blockedDomains: ["pinterest.com", "quora.com"] } },
    vfs: { tools: "edit", workspaceLinks: [MEMORY_RW] },
    subagents: { agents: [{ profileId: PROFILE.researcher }], ...RESEARCH_LIMITS },
  },
};

const METRICS_CONFIG: Record<string, unknown> = {
  model: GPT,
  generation: { reasoningEffort: "low", maxOutputTokens: 6_000 },
  limits: { maxToolRounds: 10, maxTurns: 8 },
  features: {
    mcp: { servers: [{ serverId: MCP.stripe }, { serverId: MCP.hubspot }] },
    vfs: { tools: "readOnly", workspaceLinks: [MEMORY_RO] },
  },
};

const WRITER_CONFIG: Record<string, unknown> = {
  model: SONNET,
  generation: { reasoningEffort: "high", maxOutputTokens: 16_000 },
  limits: { maxTurns: 20 },
  features: {
    vfs: { tools: "edit", workspaceLinks: [MEMORY_RO, BRIEFS_RW] },
  },
};

/// The assistant profile for a session that also reads the hiring tools:
/// the same links, HubSpot and Notion beside Google.
const HIRING_CONFIG: Record<string, unknown> = {
  ...ASSISTANT_CONFIG,
  features: {
    ...(ASSISTANT_CONFIG.features as Record<string, unknown>),
    mcp: {
      servers: [
        { serverId: MCP.google },
        { serverId: MCP.hubspot },
        { serverId: MCP.notion },
      ],
    },
  },
};

const ASSISTANT_PROFILE: ProfileInit = {
  profileId: PROFILE.assistant,
  displayName: "Executive assistant",
  description: "Ada's assistant: briefs, inbox triage with drafts for approval, meeting prep, calendar, travel, memory it keeps itself; delegates research.",
  instructions: ASSISTANT_INSTRUCTIONS,
  config: ASSISTANT_CONFIG,
  environment: ASSISTANT_ENVIRONMENT,
  revision: 17,
  createdAtMs: ago(41 * DAY_MS),
  updatedAtMs: ago(4 * DAY_MS),
};
const RESEARCHER_PROFILE: ProfileInit = {
  profileId: PROFILE.researcher,
  displayName: "Researcher",
  description: "Sourced answers from public pages; splits a question across sub-agents and writes the result under /memory/research.",
  instructions: RESEARCHER_INSTRUCTIONS,
  config: RESEARCHER_CONFIG,
  environment: { type: "inherit" },
  revision: 6,
  createdAtMs: ago(30 * DAY_MS),
  updatedAtMs: ago(9 * DAY_MS),
};
const METRICS_PROFILE: ProfileInit = {
  profileId: PROFILE.metrics,
  displayName: "Metrics analyst",
  description: "The Monday numbers from Stripe and HubSpot, and billing alerts worth Ada's attention.",
  instructions: METRICS_INSTRUCTIONS,
  config: METRICS_CONFIG,
  revision: 4,
  createdAtMs: ago(27 * DAY_MS),
  updatedAtMs: ago(13 * DAY_MS),
};
const WRITER_PROFILE: ProfileInit = {
  profileId: PROFILE.writer,
  displayName: "Briefing writer",
  description: "Longer documents from memory and briefs — board sections, investor updates — every number footnoted to its source.",
  instructions: WRITER_INSTRUCTIONS,
  config: WRITER_CONFIG,
  revision: 2,
  createdAtMs: ago(12 * DAY_MS),
  updatedAtMs: ago(12 * DAY_MS),
};

// ---------------------------------------------------------------------------
// Universe resources
// ---------------------------------------------------------------------------

function seedMembers(store: DemoStore, universe: UniverseState): void {
  universe.members.push(
    member(store, universe, "user-marco", "admin", ago(38 * DAY_MS)),
    member(store, universe, "user-priya", "member", ago(22 * DAY_MS)),
  );
  universe.apiKeys.push({
    keyPrefix: "lsk_pa_2f8d",
    displayName: "iPhone Shortcuts (voice notes → session)",
    createdAtMs: ago(19 * DAY_MS),
    lastUsedAtMs: ago(HOUR_MS + 12 * MINUTE_MS),
    revokedAtMs: null,
  });
}

function seedProfiles(universe: UniverseState): void {
  for (const init of [ASSISTANT_PROFILE, RESEARCHER_PROFILE, METRICS_PROFILE, WRITER_PROFILE]) {
    universe.profiles.set(init.profileId, profile(init));
  }
}

function seedWorkspaces(store: DemoStore, universe: UniverseState): void {
  workspace(store, universe, {
    id: WORKSPACE.memory,
    displayName: "Assistant memory",
    files: MEMORY_FILES,
    revision: 214,
    createdAtMs: ago(41 * DAY_MS),
    updatedAtMs: t(13, 41),
  });
  workspace(store, universe, {
    id: WORKSPACE.skills,
    displayName: "Assistant skills",
    files: SKILL_FILES,
    revision: 9,
    createdAtMs: ago(40 * DAY_MS),
    updatedAtMs: ago(6 * DAY_MS),
  });
  workspace(store, universe, {
    id: WORKSPACE.briefs,
    displayName: "Briefs",
    files: BRIEF_FILES,
    revision: 63,
    createdAtMs: ago(40 * DAY_MS),
    updatedAtMs: t(7, 1),
  });
}

function seedEnvironments(universe: UniverseState): void {
  const macMini: Environment = {
    environmentId: ENV_MAC_MINI,
    requestId: `req-${hex("ada-mac-mini", 12)}`,
    source: {
      type: "external",
      connection: { endpoint: "ws://mac-mini.tailnet:19091", transport: "webSocket" },
    },
    displayName: "Mac mini (home)",
    status: "ready",
    desiredPower: "running",
    incarnation: {
      incarnationId: `inc-${hex("ada-mac-mini-inc", 10)}`,
      createdAtMs: ago(35 * DAY_MS),
      updatedAtMs: ago(HOUR_MS + 42 * MINUTE_MS),
    },
    publicIngressEnabled: false,
    publicEndpoint: null,
    metadata: {
      owner: "ada",
      os: "macOS 15",
      location: "home office, Munich",
      tools: "python3, chromium (headless), trains.py, hotels.py, book.py",
    },
    createdAtMs: ago(35 * DAY_MS),
    updatedAtMs: ago(HOUR_MS + 42 * MINUTE_MS),
  };
  universe.environments.set(ENV_MAC_MINI, macMini);

  // A registration key is the group of the daemons it admitted: Ada's
  // laptop dials in from wherever she is, keeps its identity in its state
  // directory, and simply shows as offline while the lid is closed.
  const homeDevicesKey = "registration_key_home_devices";
  universe.registrationKeys.push({
    registrationKeyId: homeDevicesKey,
    displayName: "Home devices",
    keyPrefix: "lsrk_k9Qe2mB1",
    identityMode: "persistent",
    ephemeralDisconnectGraceMs: 300_000,
    status: "active",
    registeredEnvironmentCount: 1,
    activeEnvironmentCount: 1,
    lastRegisteredAtMs: ago(9 * DAY_MS),
    createdAtMs: ago(9 * DAY_MS),
  });
  const laptopDaemonId = `daemon_${hex("ada-laptop-daemon", 64)}`;
  universe.environments.set("env-ada-laptop", {
    environmentId: "env-ada-laptop",
    requestId: `daemon:${laptopDaemonId}`,
    source: {
      type: "registered",
      registrationKeyId: homeDevicesKey,
      daemonId: laptopDaemonId,
      identityMode: "persistent",
    },
    displayName: "Ada's laptop",
    status: "offline",
    desiredPower: "running",
    incarnation: {
      incarnationId: `inc-${hex("ada-laptop-inc", 10)}`,
      createdAtMs: ago(9 * DAY_MS),
      updatedAtMs: ago(3 * HOUR_MS),
    },
    publicIngressEnabled: false,
    publicEndpoint: null,
    metadata: {
      "envd.os": "macos",
      "envd.arch": "arm64",
      "lightspeed.envd.version": "0.1.0",
    },
    lastSeenAtMs: ago(3 * HOUR_MS),
    createdAtMs: ago(9 * DAY_MS),
    updatedAtMs: ago(3 * HOUR_MS),
  });
  universe.providerBindings.push(
    providerBinding({
      revision: 1,
      metadata: { project: "personal-assistant", note: "enabled for provisioned sandboxes; nothing uses it yet" },
      createdAtMs: ago(24 * DAY_MS),
      updatedAtMs: ago(24 * DAY_MS),
    }),
  );
  universe.environmentTemplates.push(
    template({
      templateId: "dev-small-v1",
      displayName: "Development VM (small)",
      description: "2 vCPU / 4 GiB, Git, Python, Node 22, envd.",
      publicIngress: true,
      deprecated: false,
      metadata: { cpu: "2", memory: "4GiB", disk: "40GiB" },
    }),
    template({
      templateId: "browser-v1",
      displayName: "Browser box",
      description: "Chromium with a desktop session for anything the Mac mini should not touch.",
      publicIngress: false,
      deprecated: false,
      metadata: { cpu: "2", memory: "8GiB", disk: "40GiB", image: "lightspeed/browser:2026.08" },
    }),
  );
}

function seedIntegrations(universe: UniverseState): void {
  for (const server of [
    mcpServer({
      serverId: MCP.google,
      displayName: "Google Workspace",
      serverUrl: `${WORKSPACE_MCP_URL}/mcp`,
      description: "Gmail and Calendar for ada@lumen.example: search, drafts, sends, events. The inbox and calendar polls read through the same server.",
      allowedTools: GOOGLE_TOOLS,
      approvalDefault: "never",
      deferLoadingDefault: false,
      authPolicy: { type: "requiredOAuth", resource: `${WORKSPACE_MCP_URL}/mcp`, scopes: ["gmail.modify", "calendar.events"] },
      credential: { type: "authGrant", grantId: GRANT.google },
      status: "active",
      revision: 6,
      createdAtMs: ago(40 * DAY_MS),
      updatedAtMs: ago(11 * DAY_MS),
    }),
    mcpServer({
      serverId: MCP.slack,
      displayName: "Slack",
      serverUrl: "https://mcp.slack.example/mcp",
      description: "Read-only on the Lumen workspace: search and channel history, for what Ada was told where.",
      allowedTools: SLACK_TOOLS,
      approvalDefault: "never",
      authPolicy: { type: "requiredBearer" },
      credential: { type: "authGrant", grantId: GRANT.slack },
      status: "active",
      revision: 3,
      createdAtMs: ago(36 * DAY_MS),
      updatedAtMs: ago(19 * DAY_MS),
    }),
    mcpServer({
      serverId: MCP.stripe,
      displayName: "Stripe",
      serverUrl: "https://mcp.stripe.example/v1",
      description: "Subscriptions, invoices, and customers for the metrics bot; restricted read-only key.",
      allowedTools: ["search_subscriptions", "list_invoices", "retrieve_invoice", "retrieve_customer", "retrieve_subscription"],
      approvalDefault: "never",
      authPolicy: { type: "requiredBearer" },
      credential: { type: "authGrant", grantId: GRANT.stripe },
      status: "active",
      revision: 2,
      createdAtMs: ago(27 * DAY_MS),
      updatedAtMs: ago(27 * DAY_MS),
    }),
    mcpServer({
      serverId: MCP.hubspot,
      displayName: "HubSpot",
      serverUrl: "https://mcp.hubspot.example/mcp",
      description: "Deals and pipelines for coverage numbers and the AE hiring pipeline Marco keeps there.",
      allowedTools: ["search_deals", "get_pipeline", "get_deal"],
      approvalDefault: "never",
      authPolicy: { type: "requiredOAuth", resource: "https://mcp.hubspot.example/mcp" },
      credential: { type: "authGrant", grantId: GRANT.hubspot },
      status: "active",
      revision: 2,
      createdAtMs: ago(26 * DAY_MS),
      updatedAtMs: ago(26 * DAY_MS),
    }),
    mcpServer({
      serverId: MCP.notion,
      displayName: "Notion",
      serverUrl: "https://mcp.notion.example/mcp",
      description: "Lumen's Notion: hiring pipeline, offsite planning, meeting notes. The workspace token was revoked when the Notion admin changed; reconnect.",
      allowedTools: ["search", "get_page", "query_database"],
      authPolicy: { type: "requiredOAuth", resource: "https://mcp.notion.example/mcp" },
      credential: { type: "authGrant", grantId: GRANT.notion },
      status: "needsAuthConfig",
      revision: 3,
      createdAtMs: ago(33 * DAY_MS),
      updatedAtMs: ago(5 * DAY_MS),
    }),
  ]) {
    universe.mcpServers.set(server.serverId, server);
  }

  const grants: SecretGrant[] = [
    {
      grantId: GRANT.google,
      providerId: MCP.google,
      providerKind: "mcpOAuth",
      displayName: "Google Workspace (ada@lumen.example)",
      subjectHint: "ada@lumen.example",
      status: "active",
      exposure: "brokered",
      principal: { kind: "user", id: "user-ada" },
      scopes: ["gmail.modify", "calendar.events"],
      audience: null,
      hasAccessToken: true,
      hasRefreshToken: true,
      expiresAtMs: NOW + 38 * MINUTE_MS,
      lastLeasedAtMs: ago(4 * MINUTE_MS),
      leaseCount: 9_812,
      createdAtMs: ago(40 * DAY_MS),
      updatedAtMs: ago(4 * MINUTE_MS),
    },
    {
      grantId: GRANT.slack,
      providerId: MCP.slack,
      providerKind: "staticBearer",
      displayName: "Slack bot token (lumen.slack)",
      subjectHint: "@ada-assistant",
      status: "active",
      exposure: "brokered",
      principal: { kind: "serviceAccount", id: BOT.assistant },
      scopes: ["search:read", "channels:history", "users:read"],
      hasAccessToken: true,
      hasRefreshToken: false,
      expiresAtMs: null,
      lastLeasedAtMs: t(13, 40),
      leaseCount: 611,
      createdAtMs: ago(36 * DAY_MS),
      updatedAtMs: ago(36 * DAY_MS),
    },
    {
      grantId: GRANT.stripe,
      providerId: MCP.stripe,
      providerKind: "staticBearer",
      displayName: "Stripe restricted key (read)",
      subjectHint: "rk_live_…7Qx2",
      status: "active",
      exposure: "retrievable",
      principal: { kind: "serviceAccount", id: BOT.metrics },
      scopes: ["subscriptions:read", "invoices:read", "customers:read"],
      hasAccessToken: true,
      hasRefreshToken: false,
      expiresAtMs: null,
      lastLeasedAtMs: at(1, 10, 2),
      leaseCount: 148,
      createdAtMs: ago(27 * DAY_MS),
      updatedAtMs: ago(27 * DAY_MS),
    },
    {
      grantId: GRANT.stripeSigning,
      providerId: "stripe-webhooks",
      providerKind: "staticBearer",
      displayName: "Stripe webhook signing secret",
      subjectHint: "metrics · stripe-webhook",
      status: "active",
      exposure: "brokered",
      principal: { kind: "serviceAccount", id: BOT.metrics },
      scopes: [],
      hasAccessToken: true,
      hasRefreshToken: false,
      expiresAtMs: null,
      lastLeasedAtMs: at(1, 10, 2),
      leaseCount: 57,
      createdAtMs: ago(27 * DAY_MS),
      updatedAtMs: ago(27 * DAY_MS),
    },
    {
      grantId: GRANT.hubspot,
      providerId: MCP.hubspot,
      providerKind: "mcpOAuth",
      displayName: "HubSpot (lumen)",
      subjectHint: "marco@lumen.example",
      status: "active",
      exposure: "brokered",
      principal: { kind: "user", id: "user-marco" },
      scopes: ["crm.objects.deals.read"],
      hasAccessToken: true,
      hasRefreshToken: true,
      expiresAtMs: NOW + 22 * MINUTE_MS,
      lastLeasedAtMs: ago(52 * MINUTE_MS),
      leaseCount: 96,
      createdAtMs: ago(26 * DAY_MS),
      updatedAtMs: ago(52 * MINUTE_MS),
    },
    {
      grantId: GRANT.notion,
      providerId: MCP.notion,
      providerKind: "mcpOAuth",
      displayName: "Notion (lumen workspace)",
      subjectHint: "ada@lumen.example",
      status: "needsReauth",
      exposure: "brokered",
      principal: { kind: "user", id: "user-ada" },
      scopes: ["read_content"],
      hasAccessToken: true,
      hasRefreshToken: true,
      expiresAtMs: ago(5 * DAY_MS),
      lastLeasedAtMs: ago(50 * MINUTE_MS),
      leaseCount: 233,
      metadata: { note: "Refresh rejected since the workspace admin changed 5 days ago; reconnect Notion on the Integrations page." },
      createdAtMs: ago(33 * DAY_MS),
      updatedAtMs: ago(5 * DAY_MS),
    },
    {
      grantId: GRANT.anthropic,
      providerId: "anthropic",
      providerKind: "modelApiKey",
      displayName: "Anthropic API key",
      subjectHint: "sk-ant-…9mA1",
      status: "active",
      exposure: "brokered",
      principal: { kind: "universeDefault" },
      hasAccessToken: true,
      hasRefreshToken: false,
      expiresAtMs: null,
      lastLeasedAtMs: ago(11 * MINUTE_MS),
      leaseCount: 6_930,
      createdAtMs: ago(41 * DAY_MS),
      updatedAtMs: ago(41 * DAY_MS),
    },
  ];
  universe.secrets = {
    providers: [
      modelProvider("anthropic", "Anthropic", { type: "modelApiKey", endpoint: null }, true, ago(41 * DAY_MS), ago(41 * DAY_MS)),
      modelProvider("openai", "OpenAI", { type: "modelApiKey", endpoint: null }, true, ago(27 * DAY_MS), ago(27 * DAY_MS)),
    ],
    grants,
  };

  const fetchedAtMs = ago(9 * MINUTE_MS);
  const efforts = ["none", "low", "medium", "high", "xhigh"];
  universe.models = {
    models: [
      modelOption(OPUS, "Claude Opus 5", { maxInputTokens: 1_000_000, maxOutputTokens: 128_000, parallelToolUse: true, reasoningEfforts: [...efforts, "max"] }, fetchedAtMs),
      modelOption(SONNET, "Claude Sonnet 5", { maxInputTokens: 1_000_000, maxOutputTokens: 64_000, parallelToolUse: true, reasoningEfforts: efforts }, fetchedAtMs),
      modelOption({ ...SONNET, model: "claude-haiku-5" }, "Claude Haiku 5", { maxInputTokens: 400_000, maxOutputTokens: 64_000, parallelToolUse: true, reasoningEfforts: efforts }, fetchedAtMs),
      modelOption(GPT, "GPT-5.4", { maxInputTokens: 272_000, maxOutputTokens: 128_000, parallelToolUse: true, reasoningEfforts: efforts }, fetchedAtMs),
    ],
    providers: [
      modelDiscovery("anthropic", ["anthropic:messages"], "configured", "universe", fetchedAtMs),
      modelDiscovery("openai", ["openai:responses"], "configured", "universe", fetchedAtMs),
    ],
  };

  const configurator: UniverseSetup = {
    id: "configurator",
    name: "Configurator",
    description: "An MCP server that lets a coding agent configure this universe — profiles, MCP servers, environments, bots — with an API key scoped to it. Ada set the assistant up through it from her laptop.",
    version: 4,
    available: true,
    status: "ready",
    installedVersion: 4,
    resources: { keyPrefix: "lsk_pa_cfg_5a19", serverId: "configurator" },
  };
  universe.setups.push(configurator);
}

// ---------------------------------------------------------------------------
// Ada's Assistant: the phone, the schedule, the inbox, the calendar
// ---------------------------------------------------------------------------

const ASSISTANT_BRIEF = `Ada's standing calendar: standup Mon–Thu 09:00 (Marco), 1:1 Marco Mon 11:00, 1:1 Priya Thu 14:00. Series A: Northlight (Elena Marsh) is the lead conversation, IC on Sep 8 if the cohort cut and the references land; Kestrel (Tom Adeyemi) — never nudge twice. Deadlines: Staff offer decision Sep 3, Q3 metrics section to Marco Sep 5, board Sep 12. Known: Notion needs re-auth (the offsite venue list is unreachable; use Marco's Slack summary). Quiet hours 23:00–08:00.`;

const HEARTBEAT_SUMMARY = "Heartbeat: run the checklist in /memory/README.md against today's log, the inbox, and the calendar; say nothing unless an item is yes.";
const MORNING_SUMMARY = "Morning brief: today's calendar with what needs prep, inbox highlights since the wrap, commitments due, travel. Write /briefs/<today>-morning.md, open today's log, send six lines.";
const EVENING_SUMMARY = "Evening wrap: what got done, what carried over, mail sent with no reply after 48 h, tomorrow's first meeting and the first one needing prep. Write /briefs/<today>-evening.md, close today's log, send six lines.";
const FRIDAY_SUMMARY = "Weekly review: follow /skills/weekly-review — commitments kept and slipped, people to follow up, next week's shape. Write /briefs/week-<n>-review.md and send six lines.";

const MONDAY_WEEK = isoWeek(at(MONDAY_DAYS_AGO, 7, 30));
const METRICS_WEEKLY_SUMMARY = `Week ${MONDAY_WEEK} numbers: MRR €412k (+3.5% w/w), net revenue churn 1.1% (was 1.4%), new customers 2, churned 1 (a trial), Q4 pipeline coverage 3.1× (was 2.9×). Largest customer Acme Corp €38.4k/mo (9.3% of MRR).`;
const METRICS_WEEKLY_DATA = {
  week: MONDAY_WEEK,
  mrr: { value: 412_000, previous: 398_000, currency: "EUR" },
  netRevenueChurn: { value: 0.011, previous: 0.014 },
  customers: { new: 2, churned: 1, total: 31 },
  pipelineCoverage: { value: 3.1, previous: 2.9, quarter: "Q4" },
  largest: { name: "Acme Corp", mrr: 38_400 },
};
const DIGEST_TEXT = `Monday numbers (week ${MONDAY_WEEK}, from metrics):
• MRR €412k, +3.5% on last week
• Net revenue churn 1.1% (was 1.4%)
• New 2 (Halden expansion, Marrow), churned 1 (a trial)
• Q4 pipeline coverage 3.1× (was 2.9×)
Acme is still 9% of MRR. MEMORY.md updated; the full digest is in the metrics bot.`;

const BILLING_RETRY_DATE = daysAgoLabel(-1);
const BILLING_ALERT_SUMMARY = `Acme Corp: invoice in_1QzT8 for €38,400 failed its 3rd payment attempt (card_declined, insufficient_funds). Stripe retries once more on ${BILLING_RETRY_DATE}; after that the subscription goes past due.`;
const BILLING_ALERT_DATA = { customer: "Acme Corp", invoice: "in_1QzT8", amount: 38_400, currency: "EUR", attempt: 3, reason: "insufficient_funds", nextAttempt: BILLING_RETRY_DATE, mrrShare: 0.093 };
const BILLING_TEXT = `Acme Corp's €38,400 invoice failed a third time (card declined). Finance already has it — Lena in #customer-acme at 08:50, Dana's office is swapping the card. Stripe retries once more on ${BILLING_RETRY_DATE}; if that fails the account goes past due. Nothing for you unless you want to call Dana yourself.`;

const OPTIONS_TEXT = `Berlin Sep 15–16, summit at Alte Münze (Molkenmarkt 2). Train both ways: ICE 08:07 Munich → 12:22 Berlin on the 15th, back 17:30 → 21:45 on the 16th, €178 with seats.
A — Hotel Luc, Molkenmarkt: 4-min walk, €189 with breakfast. Total €367.
B — Wilde Aparthotel, Klosterstraße: 6-min walk, €164, no breakfast. Total €342.
Reply A or B and I book both legs and the room.`;
const BOOKED_TEXT = `Booked A. Train DB-7K3QP2: ICE 08:07 → 12:22 on the 15th, 17:30 → 21:45 on the 16th, seats 14/61–62. Hotel Luc 15–16 Sep, confirmation LUC-48213, breakfast included. €367 on the company card. Both are in your calendar; MEMORY.md has the trip.`;

const WRAP_TEXT = `Wrap for today:
• Done: standup, 1:1 Priya (comp sheet in, rota approved 16:10), Acme renewal call.
• Carried: Acme wants a 2-year term with a price hold → Marco models it by Sep 2 (commitments.md).
• Tom Adeyemi (Kestrel): 48 h, no reply. I'll draft a nudge for the morning; you decide.
• Staff offer decision due Sep 3.
Tomorrow: standup ${clockLabel(CAL.standup)}; Northlight sync ${clockLabel(CAL.northlight)} — prep at ${hm(9, 50)}. 18 quiet heartbeats today.`;

const MORNING_TEXT = `Good morning — ${weekdayName(NOW)} ${TODAY}.
• ${clockLabel(CAL.standup)} standup · ${clockLabel(CAL.northlight)} Northlight sync (prep at ${hm(9, 50)}) · ${clockLabel(CAL.priya)} Priya · ${clockLabel(CAL.debrief)} candidate debrief (prep ${hm(15, 20)})
• Inbox: 11 unread, 1 important — Sam confirming today's agenda, FYI. 6 archived.
• Tom Adeyemi (Kestrel): 48 h, no reply. A nudge is drafted, not sent — say *send it*, or leave it (investors.md: never nudge twice).
• Due today: cohort cut and references to Elena — the cut is in the data room, references are cleared (investors.md).
• No travel. Full brief: /briefs/${TODAY}-morning.md`;

const PREP_TEXT = `Northlight sync in 40 min (${clockLabel(CAL.northlight)}, Meet link in the invite).
• Elena Marsh (partner, lead) and Sam Okoro (associate, runs the data-room questions).
• Last: partner meeting ${daysAgoLabel(9)}, positive — "term sheet discussion after references" (people/investors.md). Sam's mail ${daysAgoLabel(3)}: cohort cut "before Thursday" — you promised it for today (commitments.md).
• Open: the cohort cut is in the data room; three references cleared — Dana (Acme), Ines (Halden), Kofi (Marrow). Q3 pipeline coverage: Marco is still preparing it.
• Opening question: "Sam, did the 2025 cohorts answer the net-retention question, or do you want the cut by product too?"`;

const RESCHEDULE_TEXT = `Done: 1:1 Priya moved to tomorrow ${clockLabel(CAL.priya)}, same room link; her invite is updated and she is free then (checked). Marco's ${MONDAY} note says that makes four this month — expect to hear about it at standup.
Pricing: research has it (their #${SEQ.researchRequest}) — Beacon and Orbit with our own page as the baseline, due by lunch. I'll send the reading when it lands.`;

const RESEARCH_TEXT = `Competitor pricing is in /memory/research/competitor-pricing.md. The reading:
• Lumen Business at €89/seat is the most expensive per seat but the only tier that scales with usage — the deck's expansion claim stands.
• Beacon's free-forever tier (3 users) is the funnel to beat; their March rise to $39 stuck.
• Orbit is seats only: no usage component, no enterprise floor published.
Two gaps named in the file: Orbit's enterprise pricing and Beacon's enterprise minimum seats.`;

const ELENA_MAIL_ID = "19b4c7e0a2d13f58";
const ELENA_THREAD_ID = "19b3a1f7c02e4d90";
const ELENA_SNIPPET = "Thanks for this morning. Two things before we take you to IC on the 8th: (1) the cohort retention cut by month for the 2025 cohorts, (2) three customer references we can call this week. If both land by Thursday I can put you on the agenda.";
const ELENA_BODY = `Ada,

${ELENA_SNIPPET}

Sam will send the data-room questions on gross margin separately; nothing blocking there.

Elena`;
const ELENA_DRAFT_ID = "r-2874591036";
const ELENA_DRAFT = `Elena — thanks for the time this morning.

Both are here: the 2025 monthly cohort retention cut is attached (cohort-retention-2025.csv, the same file that is in the data room), and three references who are expecting a call this week — Dana Whitfield (CTO, Acme Corp), Ines Brandt (COO, Halden Logistics), Kofi Mensah (VP Ops, Marrow Health). Anything else Sam needs for IC, send it my way.

Ada`;
const TRIAGE_TEXT = `Elena (Northlight), ${hm(11, 17)}, needs a reply: she wants the 2025 cohort retention cut and three references before IC on Sep 8 — both by Thursday puts you on the agenda.
Draft ready: thanks for the time, the cut attached (cohort-retention-2025.csv from the data room), the three references with roles (investors.md), "anything else Sam needs, send it my way", signed Ada. Five lines.
Say *send it* and I'll send it. Nothing else important in the inbox.`;
const SENT_TEXT = `Sent to Elena at ${hm(11, 25)} with the cut and the three references. commitments.md: that row is done. Kestrel's nudge is still the only draft waiting on you.`;

const PROMISE_TEXT = `From commitments.md, this week, to Priya:
• Decide on the Staff engineer offer — by Sep 3 (comp sheet arrived yesterday; the candidate's other deadline is Sep 4).
• Review the on-call rota change before it goes to the team — done yesterday 16:10 in #eng-leads (checked in Slack).
• Introduce her to Kestrel's CTO-in-residence (Lena Fischer) — by Sep 5, waiting on Tom's reply.
And from today's log: her 1:1 is tomorrow ${clockLabel(CAL.priya)}, not today.`;

const CALENDAR_TODAY = {
  events: [
    { id: "evt_standup_" + dateLabel(NOW), summary: "Leadership standup", start: atIso(CAL.standup), end: atIso(CAL.standup + 30 * MINUTE_MS), attendees: ["marco@lumen.example", "priya@lumen.example", "ada@lumen.example"], organizer: "marco@lumen.example" },
    { id: "evt_northlight_sync", summary: "Investor sync — Northlight Ventures", start: atIso(CAL.northlight), end: atIso(CAL.northlight + 45 * MINUTE_MS), attendees: ["elena@northlight.example", "sam@northlight.example", "ada@lumen.example"], hangoutLink: "https://meet.google.example/nlv-sync", organizer: "ada@lumen.example" },
    { id: "evt_1on1_priya", summary: "1:1 Priya", start: atIso(CAL.priya), end: atIso(CAL.priya + 50 * MINUTE_MS), attendees: ["priya@lumen.example", "ada@lumen.example"], organizer: "ada@lumen.example" },
    { id: "evt_debrief_staff", summary: "Candidate debrief — Staff engineer", start: atIso(CAL.debrief), end: atIso(CAL.debrief + 25 * MINUTE_MS), attendees: ["priya@lumen.example", "marco@lumen.example", "ada@lumen.example"], organizer: "priya@lumen.example" },
  ],
};

const INBOX_MORNING = {
  resultSizeEstimate: 11,
  messages: [
    { id: "19b4b2d8e0a71c33", from: "Sam Okoro <sam@northlight.example>", subject: "Agenda for today", labels: ["IMPORTANT", "INBOX", "UNREAD"], snippet: "Confirming 10:30: cohorts, references, gross margin. Elena joins for the first 30." },
    { id: "19b4b0a1c7f52e19", from: "Stripe <notifications@stripe.example>", subject: "Payout of €61,240.00 sent", labels: ["INBOX", "UNREAD", "CATEGORY_UPDATES"], snippet: "Your payout is on its way" },
    { id: "19b4ae9f3b1d0c77", from: "Lena Hoff <lena@lumen.example>", subject: "Fwd: office lease renewal", labels: ["INBOX", "UNREAD"], snippet: "FYI only, Marco has it" },
  ],
  archivedByRule: 6,
};

const SENT_NO_REPLY = {
  messages: [
    { id: "19b3f0c2a9d84e11", to: "tom@kestrel.example", subject: "Series A — follow-up call", sentAt: agoIso(3 * DAY_MS + 2 * HOUR_MS), replies: 0, ageHours: 72 },
  ],
};

const TOM_DRAFT_ID = "r-2874588120";
const TOM_DRAFT = `Tom — still keen on a call the week of Sep 7 if that works for you; I have Tue and Thu open. No rush on the gross-margin questions; Sam has the same ones.

Ada`;

function seedAssistant(store: DemoStore, universe: UniverseState): void {
  const record = bot(universe, {
    botId: BOT.assistant,
    displayName: "Ada's Assistant",
    description: "Ada's always-on assistant on Telegram and WhatsApp: briefs, inbox triage with drafts for approval, meeting prep, calendar, travel, its own memory; asks research and metrics for what it cannot do itself.",
    profileId: PROFILE.assistant,
    brief: ASSISTANT_BRIEF,
    runsPerDay: 150,
    breaker: { fires: 20, windowMs: 10 * MINUTE_MS },
    selfConfig: true,
    emit: true,
    createdAtMs: ago(41 * DAY_MS),
    updatedAtMs: ago(4 * DAY_MS),
  });

  const toAda = { route: { policy: "perKey" as const, key: "ada" }, deliver: { whenBusy: "queue" as const }, sessionCloseAfterMs: 0 };
  const triggers = new Map([
    [
      "telegram",
      chatTrigger(
        BOT.assistant,
        "telegram",
        {
          accountId: TELEGRAM_ADA_ACCOUNT_ID,
          matchScope: "direct",
          activation: null,
          access: { turn: "anyone" },
          pairingCode: "ADA-7Q2K",
          priority: 10,
        },
        { createdAtMs: ago(41 * DAY_MS), updatedAtMs: ago(33 * DAY_MS), deliver: { whenBusy: "queue" } },
      ),
    ],
    [
      "whatsapp",
      chatTrigger(
        BOT.assistant,
        "whatsapp",
        {
          accountId: WHATSAPP_ADA_ACCOUNT_ID,
          matchScope: "direct",
          activation: null,
          access: { turn: "anyone" },
          pairingCode: "ADA-WA-3M8P",
          priority: 20,
        },
        { createdAtMs: ago(26 * DAY_MS), updatedAtMs: ago(25 * DAY_MS), deliver: { whenBusy: "queue" } },
      ),
    ],
    [
      "heartbeat",
      scheduleTrigger(
        BOT.assistant,
        "heartbeat",
        { cron: "*/30 8-21 * * 1-5", summary: HEARTBEAT_SUMMARY },
        { ...toAda, createdAtMs: ago(40 * DAY_MS), updatedAtMs: ago(15 * DAY_MS) },
      ),
    ],
    ["morning-brief", scheduleTrigger(BOT.assistant, "morning-brief", { cron: "0 7 * * 1-5", summary: MORNING_SUMMARY }, { ...toAda, createdAtMs: ago(41 * DAY_MS), updatedAtMs: ago(15 * DAY_MS) })],
    ["evening-wrap", scheduleTrigger(BOT.assistant, "evening-wrap", { cron: "30 17 * * 1-5", summary: EVENING_SUMMARY }, { ...toAda, createdAtMs: ago(41 * DAY_MS), updatedAtMs: ago(15 * DAY_MS) })],
    ["friday-review", scheduleTrigger(BOT.assistant, "friday-review", { cron: "0 16 * * 5", summary: FRIDAY_SUMMARY }, { ...toAda, createdAtMs: ago(34 * DAY_MS), updatedAtMs: ago(6 * DAY_MS) })],
    [
      "inbox-poll",
      pollTrigger(
        BOT.assistant,
        "inbox-poll",
        {
          source: {
            kind: "http",
            url: `${WORKSPACE_MCP_URL}/gmail/messages?q=is%3Aunread+label%3AIMPORTANT+newer_than%3A1d`,
            method: "GET",
            headers: { accept: "application/json" },
            auth: { grantId: GRANT.google, header: "authorization", scheme: "Bearer" },
          },
          intervalMs: 10 * MINUTE_MS,
          items: "$.messages[*]",
          cursor: { kind: "idSet", id: "id" },
        },
        {
          ...toAda,
          filter: 'data.labels.contains("IMPORTANT") && !data.fromSelf',
          cursorState: {
            ids: [ELENA_MAIL_ID, "19b4b2d8e0a71c33", "19b3f0c2a9d84e11", "19b39a4e6c08d2b5"],
            consecutiveFailures: 0,
            baselinedAtMs: ago(40 * DAY_MS),
            lastPolledAtMs: ago(3 * MINUTE_MS),
          },
          createdAtMs: ago(40 * DAY_MS),
          updatedAtMs: ago(15 * DAY_MS),
        },
      ),
    ],
    [
      "calendar-poll",
      pollTrigger(
        BOT.assistant,
        "calendar-poll",
        {
          source: {
            kind: "http",
            url: `${WORKSPACE_MCP_URL}/calendar/events?calendarId=primary&window=40m&singleEvents=true`,
            method: "GET",
            headers: { accept: "application/json" },
            auth: { grantId: GRANT.google, header: "authorization", scheme: "Bearer" },
          },
          intervalMs: 15 * MINUTE_MS,
          items: "$.events[*]",
          cursor: { kind: "idSet", id: "id" },
        },
        {
          ...toAda,
          filter: "data.startsInMinutes <= 40 && !data.prepped",
          cursorState: {
            ids: ["evt_northlight_sync", "evt_acme_renewal", "evt_1on1_priya"],
            consecutiveFailures: 0,
            baselinedAtMs: ago(40 * DAY_MS),
            lastPolledAtMs: ago(7 * MINUTE_MS),
          },
          createdAtMs: ago(40 * DAY_MS),
          updatedAtMs: ago(15 * DAY_MS),
        },
      ),
    ],
    ["inbox", inboxTrigger(BOT.assistant, [BOT.research, BOT.metrics], { ...toAda, createdAtMs: ago(30 * DAY_MS), updatedAtMs: ago(27 * DAY_MS) })],
  ]);

  // The numbered log, oldest first: last Monday's numbers, a billing alert,
  // the Berlin trip on WhatsApp, yesterday's wrap, then today from 07:00.
  const log = eventLog(store, BOT.assistant);
  const fromBot = (sender: string, kind: string, summary: string, atMs: number, detail: string, data: unknown, resolvedAfterMs = 60_000): ScriptedEvent =>
    log.add({
      kind,
      source: `bot:${sender}`,
      at: atMs,
      summary,
      eventId: `bot:${sender}:${hex(`${kind}:${summary}`, 24)}`,
      session: TELEGRAM,
      sender,
      hops: 1,
      outcome: "handled",
      detail,
      resolvedAfterMs,
      data,
    });
  const schedule = (name: string, summary: string, atMs: number, outcome: "handled" | "ignored", detail: string, resolvedAfterMs = 45_000): ScriptedEvent =>
    log.add({
      kind: "schedule",
      source: `schedule:${name}`,
      at: atMs,
      summary,
      eventId: `schedule:${name}:${atIso(atMs)}`,
      session: TELEGRAM,
      outcome,
      detail,
      resolvedAfterMs,
      data: { trigger: name, scheduledAt: atIso(atMs), timezone: "Europe/Berlin" },
    });

  const e1 = fromBot(BOT.metrics, "metrics.weekly", METRICS_WEEKLY_SUMMARY, at(MONDAY_DAYS_AGO, 7, 31), `Relayed the six numbers to Ada (#2); MEMORY.md company facts updated`, METRICS_WEEKLY_DATA);
  const e2 = chatSent(log, TELEGRAM, DIGEST_TEXT, at(MONDAY_DAYS_AGO, 7, 33), null);
  const e3 = fromBot(BOT.metrics, "billing.alert", BILLING_ALERT_SUMMARY, at(2, 9, 12), "Told Ada (#4): Dana's card, finance already on it in #customer-acme since 08:50; nothing for Ada unless the last attempt fails", BILLING_ALERT_DATA);
  const e4 = chatSent(log, TELEGRAM, BILLING_TEXT, at(2, 9, 13), null);
  const e5 = chatMessage(log, WHATSAPP, "whatsapp", "Ada Demo", "Berlin, 15–16 Sep, near the venue. Train not plane. Two options please.", at(2, 12, 40), "handled", "Two options from trains.py and the hotel search on the Mac mini: A Hotel Luc €189 with breakfast, B Wilde Aparthotel €164; same train both ways");
  const e6 = chatSent(log, WHATSAPP, OPTIONS_TEXT, at(2, 12, 43), e5.envelope.seq ?? 0);
  const e7 = chatMessage(log, WHATSAPP, "whatsapp", "Ada Demo", "A. Book it.", at(2, 12, 51), "handled", "Booked A: train DB-7K3QP2 and Hotel Luc LUC-48213, €367; two calendar events; MEMORY.md updated");
  const e8 = chatSent(log, WHATSAPP, BOOKED_TEXT, at(2, 12, 53), e7.envelope.seq ?? 0);
  const e9 = schedule("heartbeat", HEARTBEAT_SUMMARY, at(1, 14, 30), "ignored", "Nothing new since 14:00; 1:1 Priya in progress; next prep due 15:20 (Acme call)", 12_000);
  const e10 = schedule("evening-wrap", EVENING_SUMMARY, at(1, 17, 30), "handled", `Wrap sent (#11): 3 done, 2 carried; Kestrel at 48 h without a reply → nudge to draft in the morning; tomorrow's first meeting ${clockLabel(CAL.standup)} standup`, 70_000);
  const e11 = chatSent(log, TELEGRAM, WRAP_TEXT, at(1, 17, 31), null);
  const e12 = schedule("morning-brief", MORNING_SUMMARY, t(7, 0), "handled", `Brief sent (#13); Kestrel nudge drafted, not sent; /briefs/${TODAY}-morning.md and today's log written`, 95_000);
  const e13 = chatSent(log, TELEGRAM, MORNING_TEXT, t(7, 2), null);
  const e14 = schedule("heartbeat", HEARTBEAT_SUMMARY, t(8, 30), "ignored", `Brief went out at ${hm(7, 2)}; nothing past its time; Northlight prep due ${hm(9, 50)}`, 11_000);
  const e15 = log.add({
    kind: "calendar.upcoming",
    source: "poll:calendar-poll",
    at: t(9, 50),
    summary: `Investor sync — Northlight Ventures starts in 40 min (${clockLabel(CAL.northlight)}–${clockLabel(CAL.northlight + 45 * MINUTE_MS)}): Elena Marsh, Sam Okoro, Ada Demo · Google Meet`,
    eventId: "poll:calendar-poll:evt_northlight_sync",
    session: TELEGRAM,
    outcome: "handled",
    detail: "Prep sent (#16) from people/investors.md and the last Northlight thread; event marked prepped",
    resolvedAfterMs: 55_000,
    data: { id: "evt_northlight_sync", summary: "Investor sync — Northlight Ventures", start: atIso(CAL.northlight), end: atIso(CAL.northlight + 45 * MINUTE_MS), attendees: ["elena@northlight.example", "sam@northlight.example", "ada@lumen.example"], startsInMinutes: 40, prepped: false, hangoutLink: "https://meet.google.example/nlv-sync" },
  });
  const e16 = chatSent(log, TELEGRAM, PREP_TEXT, t(9, 51), null);
  const e17 = chatMessage(log, TELEGRAM, "telegram", "Ada Demo", "Move my 2pm with Priya to tomorrow, same slot. And get me competitor pricing for the Series A deck by lunch — Beacon and Orbit at least.", t(10, 6), "handled", `Priya's 1:1 moved to tomorrow ${clockLabel(CAL.priya)} (internal, no yes needed; invite updated); research asked for the pricing (#${SEQ.researchRequest} there), receipt requested`);
  const e18 = chatSent(log, TELEGRAM, RESCHEDULE_TEXT, t(10, 8), e17.envelope.seq ?? 0);
  const e19 = schedule("heartbeat", HEARTBEAT_SUMMARY, t(11, 0), "handled", `Research #${SEQ.researchRequest} pending 53 min with no receipt: nudged (#${SEQ.researchNudge} there). Nothing for Ada.`, 14_000);
  const e20 = receipt(log, {
    from: BOT.research,
    askedSeq: SEQ.researchRequest,
    status: "handled",
    summary: "Wrote /memory/research/competitor-pricing.md: Lumen vs Beacon Analytics vs Orbit Insights — tiers, usage pricing, discounts, last price change; two gaps named",
    at: t(11, 9),
    hops: 2,
    session: TELEGRAM,
    outcome: "handled",
    detail: "Read the file; sent Ada the three-line reading (#21); today's log updated",
  });
  const e21 = chatSent(log, TELEGRAM, RESEARCH_TEXT, t(11, 10), null);
  const e22 = log.add({
    kind: "mail.important",
    source: "poll:inbox-poll",
    at: t(11, 17),
    summary: `Elena Marsh <elena@northlight.example> · Re: today's sync — follow-ups · "${ELENA_SNIPPET.slice(0, 118)}…"`,
    eventId: `poll:inbox-poll:${ELENA_MAIL_ID}`,
    session: TELEGRAM,
    outcome: "handled",
    detail: "Needs reply: drafted the reply with the cohort cut attached and three references from people/investors.md; asked Ada to say send it (#23)",
    resolvedAfterMs: 75_000,
    data: { id: ELENA_MAIL_ID, threadId: ELENA_THREAD_ID, from: "Elena Marsh <elena@northlight.example>", to: "ada@lumen.example", subject: "Re: today's sync — follow-ups", snippet: ELENA_SNIPPET, labels: ["IMPORTANT", "INBOX", "UNREAD"], fromSelf: false, receivedAt: atIso(t(11, 16)) },
  });
  const e23 = chatSent(log, TELEGRAM, TRIAGE_TEXT, t(11, 18), null);
  const e24 = chatMessage(log, TELEGRAM, "telegram", "Ada Demo", "send it", t(11, 24), "handled", "Sent the draft to Elena (gmail.send); commitments.md row done; investors.md and today's log updated");
  const e25 = chatSent(log, TELEGRAM, SENT_TEXT, t(11, 25), e24.envelope.seq ?? 0);
  const e26 = chatMessage(log, TELEGRAM, "telegram", "Ada Demo", "what did I promise Priya this week?", t(13, 40), "handled", "Three rows from commitments.md (offer decision Sep 3, rota review done, Kestrel CTO intro Sep 5) plus the moved 1:1; the rota check confirmed in Slack; file cited");
  const e27 = chatSent(log, TELEGRAM, PROMISE_TEXT, t(13, 41), e26.envelope.seq ?? 0);
  const e28 = schedule("heartbeat", HEARTBEAT_SUMMARY, t(14, 30), "ignored", `Nothing past its time; Elena's reply went ${hm(11, 25)}; research landed; next prep ${hm(15, 20)} (candidate debrief)`, 13_000);

  // Sessions: the main one, the Telegram thread (keyed `ada`, where the
  // schedules land too), and the WhatsApp thread.
  const threadTools = [...BOT_TOOLS, ...SELF_CONFIG_TOOLS, EMIT_TOOL, ...MESSAGE_TOOLS];
  const mainSession = managedSession(store, universe, {
    id: SESSION.assistantMain,
    botId: BOT.assistant,
    displayName: "Ada's Assistant",
    profile: ASSISTANT_PROFILE,
    tools: [...BOT_TOOLS, ...SELF_CONFIG_TOOLS, EMIT_TOOL],
    createdAtMs: ago(41 * DAY_MS),
    environmentId: ENV_MAC_MINI,
  });
  appendScriptedRun(store, mainSession, {
    at: ago(41 * DAY_MS) + 20_000,
    user: INTRODUCTION_PROMPT,
    steps: [
      {
        tools: [vfsReadFile("/memory/README.md", MEMORY_README)],
        text: "I am Ada's assistant. I wake on her Telegram and WhatsApp messages (one thread each, kept), on a 30-minute heartbeat during the day where I check the list in /memory/README.md and usually say nothing, on the morning brief, the evening wrap, and the Friday review, and on two polls — important unread mail and meetings starting within 40 minutes — plus whatever research and metrics send me. I read and write my memory under /memory, follow the playbooks under /skills, use Gmail and Calendar and Slack, and run scripts on the Mac mini at home. I draft mail and never send it without \"send it\"; I move internal meetings on my own and ask for anything with an outsider.\n\nTwo things to confirm: the WhatsApp number is not paired yet, and preferences.md has no quiet-hours line — I will assume 23:00–08:00 until you say otherwise.",
      },
    ],
  });

  const telegram = managedSession(store, universe, {
    id: SESSION.telegram,
    botId: BOT.assistant,
    displayName: TELEGRAM.label,
    profile: ASSISTANT_PROFILE,
    tools: threadTools,
    createdAtMs: ago(33 * DAY_MS),
    environmentId: ENV_MAC_MINI,
  });
  const whatsapp = managedSession(store, universe, {
    id: SESSION.whatsapp,
    botId: BOT.assistant,
    displayName: WHATSAPP.label,
    profile: ASSISTANT_PROFILE,
    tools: threadTools,
    createdAtMs: ago(25 * DAY_MS),
    environmentId: ENV_MAC_MINI,
  });

  e1.envelope.runId = appendScriptedRun(store, telegram, {
    at: at(MONDAY_DAYS_AGO, 7, 31),
    user: e1.prompt,
    steps: [
      {
        thinking: "The Monday numbers, half an hour after the brief. Six numbers, same order as always; the only change worth a line is churn coming down. Update the company facts in MEMORY.md, then six lines to Ada.",
        tools: [vfsReadFile("/memory/MEMORY.md", MEMORY_BEFORE_DIGEST), vfsWriteFile("/memory/MEMORY.md", MEMORY_CURRENT, "MRR, churn, digest date")],
      },
      {
        tools: [messageSend(TELEGRAM, DIGEST_TEXT, null, e2.envelope.seq ?? 0)],
        text: `Relayed the week ${MONDAY_WEEK} numbers to Ada (#${e2.envelope.seq}); MEMORY.md company facts now say €412k and 1.1%. Nothing to act on.`,
      },
    ],
  }).id;
  e3.envelope.runId = appendScriptedRun(store, telegram, {
    at: at(2, 9, 12),
    user: e3.prompt,
    steps: [
      {
        thinking: "Acme is the largest customer (MEMORY.md: 9% of MRR) and this is the third attempt — metrics was right to flag it. Before I interrupt Ada's morning: is finance already on it?",
        tools: [
          vfsReadFile("/memory/MEMORY.md", MEMORY_CURRENT),
          mcpCall("slack.search_messages", { query: "Acme card in:#customer-acme after:yesterday", count: 5 }, { matches: [{ channel: "#customer-acme", user: "Lena Hoff", ts: atIso(at(2, 8, 50)), text: "Acme's card declined again (3rd). Dana's office is issuing a new card today; I'll ask Stripe support to hold the dunning email." }], total: 1 }),
        ],
      },
      {
        tools: [messageSend(TELEGRAM, BILLING_TEXT, null, e4.envelope.seq ?? 0)],
        text: `Told Ada (#${e4.envelope.seq}): finance has had it since 08:50, a new card is coming, the last retry is ${BILLING_RETRY_DATE}. Not quiet hours, and it is the largest customer, so it was worth a message; nothing for her to do.`,
      },
    ],
  }).id;

  e5.envelope.runId = appendScriptedRun(store, whatsapp, {
    at: at(2, 12, 40),
    user: e5.prompt,
    steps: [
      {
        thinking: "Travel: follow the playbook. The venue and dates are in MEMORY.md (summit, Alte Münze). Trains have no API — trains.py on the Mac mini; hotels through the aggregator page, then live rates on two.",
        tools: [
          vfsReadFile("/skills/travel/SKILL.md", SKILL_TRAVEL),
          vfsReadFile("/memory/preferences.md", PREFERENCES),
          mac(["python3", "trains.py", "München Hbf", "Berlin Hbf", "2026-09-15", "--arrive-by", "12:30", "--return", "2026-09-16", "--depart-after", "17:00"], JSON.stringify({ outbound: [{ train: "ICE 1006", dep: "08:07", arr: "12:22", duration: "4:15", price: 89, seats: "available" }], inbound: [{ train: "ICE 1007", dep: "17:30", arr: "21:45", duration: "4:15", price: 89, seats: "available" }], total: 178 }, null, 2)),
          webFetch("https://hotels.example/search?near=Molkenmarkt+2%2C+Berlin&checkin=2026-09-15&nights=1&max=220", "aggregator search", "12 results within 1 km. Under €220 and above 8.0: Hotel Luc (Molkenmarkt, 0.3 km, from €189, breakfast), Wilde Aparthotel (Klosterstraße, 0.5 km, from €164), Hotel Nikolai (Poststraße, 0.4 km, from €212)…"),
          mac(["python3", "hotels.py", "--near", "Molkenmarkt 2, Berlin", "--checkin", "2026-09-15", "--nights", "1", "--max", "220", "--only", "Hotel Luc,Wilde Aparthotel"], JSON.stringify({ options: [{ hotel: "Hotel Luc", walkMin: 4, rate: 189, breakfast: true, cancelUntil: "2026-09-13" }, { hotel: "Wilde Aparthotel", walkMin: 6, rate: 164, breakfast: false, cancelUntil: "2026-09-14" }] }, null, 2)),
        ],
      },
      {
        tools: [messageSend(WHATSAPP, OPTIONS_TEXT, e5.envelope.seq ?? 0, e6.envelope.seq ?? 0)],
        text: `Two options sent (#${e6.envelope.seq}): the same ICE both ways, Hotel Luc with breakfast at €189 or the Wilde Aparthotel at €164. Nothing booked until she picks.`,
      },
    ],
  }).id;
  e7.envelope.runId = appendScriptedRun(store, whatsapp, {
    at: at(2, 12, 51),
    user: e7.prompt,
    steps: [
      {
        thinking: "\"A. Book it.\" is the yes. Book both legs and the room on the Mac mini, put both in the calendar with the confirmation numbers, and write the trip into MEMORY.md.",
        tools: [
          mac(["python3", "book.py", "--option", "A", "--train", "ICE 1006/1007", "--hotel", "Hotel Luc", "--card", "company"], JSON.stringify({ train: { booking: "DB-7K3QP2", seats: ["14/61", "14/62"], price: 178 }, hotel: { confirmation: "LUC-48213", nights: 1, price: 189, breakfast: true }, total: 367 }, null, 2)),
          mcpCall("calendar.create_event", { calendarId: "primary", summary: "Train Munich → Berlin (ICE 1006, DB-7K3QP2)", start: "2026-09-15T08:07:00+02:00", end: "2026-09-15T12:22:00+02:00", description: "Seats 14/61–62. Return ICE 1007 16 Sep 17:30 → 21:45." }, { id: "evt_train_out_0915", status: "confirmed" }),
          mcpCall("calendar.create_event", { calendarId: "primary", summary: "Hotel Luc, Berlin (LUC-48213)", start: "2026-09-15", end: "2026-09-17", description: "Molkenmarkt, 4 min to the venue. Breakfast included. Free cancellation until Sep 13." }, { id: "evt_hotel_luc_0915", status: "confirmed" }),
          vfsWriteFile("/memory/MEMORY.md", MEMORY_CURRENT, "Berlin trip: train and Hotel Luc booked"),
        ],
      },
      {
        tools: [messageSend(WHATSAPP, BOOKED_TEXT, e7.envelope.seq ?? 0, e8.envelope.seq ?? 0)],
        text: `Booked A (#${e8.envelope.seq}): DB-7K3QP2 and LUC-48213, €367 on the company card. Two calendar events; MEMORY.md has the trip line.`,
      },
    ],
  }).id;

  e9.envelope.runId = appendScriptedRun(store, telegram, {
    at: at(1, 14, 30),
    user: e9.prompt,
    steps: [{ text: "Checklist: nothing past its time in today's log, no sent mail past 48 h yet (Kestrel reaches 48 h tonight), the 1:1 with Priya is in progress, the next prep is due 15:20. Nothing to say." }],
  }).id;
  e10.envelope.runId = appendScriptedRun(store, telegram, {
    at: at(1, 17, 30),
    user: e10.prompt,
    steps: [
      {
        thinking: "Wrap: read today's log for what got done, tomorrow's calendar for the first meeting and the first prep, and sent mail without a reply — Tom Adeyemi crosses 48 h today.",
        tools: [
          vfsReadFile(`/memory/daily/${YESTERDAY}.md`, DAILY_YESTERDAY),
          mcpCall("calendar.list_events", { calendarId: "primary", timeMin: atIso(at(0, 0, 0)), timeMax: atIso(at(0, 23, 59)), singleEvents: true }, CALENDAR_TODAY),
          mcpCall("gmail.search", { q: "in:sent newer_than:3d -has:replies", maxResults: 10 }, SENT_NO_REPLY),
        ],
      },
      {
        tools: [
          vfsWriteFile(`/briefs/${YESTERDAY}-evening.md`, BRIEF_EVENING, "wrap"),
          vfsWriteFile(`/memory/daily/${YESTERDAY}.md`, DAILY_YESTERDAY, "closed: wrap sent, Kestrel carried"),
          messageSend(TELEGRAM, WRAP_TEXT, null, e11.envelope.seq ?? 0),
        ],
        text: `Wrap sent (#${e11.envelope.seq}): three done, two carried. Tom Adeyemi is at 48 h without a reply, so the morning brief gets a drafted nudge — not sent, per investors.md. Tomorrow's first meeting is the ${clockLabel(CAL.standup)} standup; the Northlight sync at ${clockLabel(CAL.northlight)} is the one needing prep.`,
      },
    ],
  }).id;

  e12.envelope.runId = appendScriptedRun(store, telegram, {
    at: t(7, 0),
    user: e12.prompt,
    steps: [
      {
        thinking: "Morning brief. Calendar for today, inbox since the wrap, yesterday's log for what carried over, commitments due today. The wrap said to draft the Kestrel nudge — draft it, do not send it.",
        tools: [
          mcpCall("calendar.list_events", { calendarId: "primary", timeMin: atIso(at(0, 0, 0)), timeMax: atIso(at(0, 23, 59)), singleEvents: true }, CALENDAR_TODAY),
          mcpCall("gmail.search", { q: "is:unread newer_than:1d -category:promotions", maxResults: 20 }, INBOX_MORNING),
          vfsReadFile(`/memory/daily/${YESTERDAY}.md`, DAILY_YESTERDAY),
          vfsReadFile("/memory/commitments.md", COMMITMENTS_MORNING),
          vfsReadFile("/memory/people/investors.md", PEOPLE_INVESTORS),
        ],
      },
      {
        tools: [
          mcpCall("gmail.create_draft", { threadId: "19b3f0c2a9d84e11", to: "tom@kestrel.example", subject: "Re: Series A — follow-up call", body: TOM_DRAFT }, { draftId: TOM_DRAFT_ID, threadId: "19b3f0c2a9d84e11" }),
          mcpCall("gmail.modify_labels", { ids: ["19b4b0a1c7f52e19", "19b4ae9f3b1d0c77"], remove: ["INBOX"], add: ["assistant/archived"] }, { modified: 2 }),
          vfsWriteFile(`/briefs/${TODAY}-morning.md`, BRIEF_MORNING, "today's brief"),
          vfsWriteFile(`/memory/daily/${TODAY}.md`, DAILY_TODAY_OPEN, "opened today's log"),
        ],
      },
      {
        tools: [messageSend(TELEGRAM, MORNING_TEXT, null, e13.envelope.seq ?? 0)],
        text: `Brief sent (#${e13.envelope.seq}). Four meetings, one needing prep at ${hm(9, 50)}; one FYI mail; the Kestrel nudge is drafted and waiting on Ada. /briefs/${TODAY}-morning.md written and today's log opened.`,
      },
    ],
  }).id;
  e14.envelope.runId = appendScriptedRun(store, telegram, {
    at: t(8, 30),
    user: e14.prompt,
    steps: [{ text: `Checklist: nothing past its time, the brief went out at ${hm(7, 2)}, Kestrel's nudge is already with Ada, the Northlight prep is due ${hm(9, 50)} and calendar-poll will deliver it. Nothing to say.` }],
  }).id;
  e15.envelope.runId = appendScriptedRun(store, telegram, {
    at: t(9, 50),
    user: e15.prompt,
    steps: [
      {
        thinking: "Meeting prep, 40 minutes out. Playbook: attendees, last interaction, open threads, one question. Elena and Sam are in people/investors.md; the open asks are in commitments.md; check the last thread for anything the file does not have.",
        tools: [
          vfsReadFile("/skills/meeting-prep/SKILL.md", SKILL_MEETING_PREP),
          vfsReadFile("/memory/people/investors.md", PEOPLE_INVESTORS),
          vfsReadFile("/memory/commitments.md", COMMITMENTS_MORNING),
          mcpCall("gmail.search", { q: "from:northlight.example newer_than:14d", maxResults: 5 }, { messages: [{ id: "19b4b2d8e0a71c33", from: "Sam Okoro", subject: "Agenda for today", date: atIso(t(6, 48)) }, { id: "19b39a4e6c08d2b5", from: "Sam Okoro", subject: "Data room — cohort cut", date: agoIso(3 * DAY_MS + 5 * HOUR_MS) }], resultSizeEstimate: 2 }),
        ],
      },
      {
        tools: [
          messageSend(TELEGRAM, PREP_TEXT, null, e16.envelope.seq ?? 0),
          mcpCall("calendar.update_event", { calendarId: "primary", eventId: "evt_northlight_sync", extendedProperties: { private: { prepped: "true" } } }, { id: "evt_northlight_sync", updated: true }),
          vfsWriteFile(`/memory/daily/${TODAY}.md`, DAILY_TODAY_OPEN.replace(`- [ ] ${clockLabel(CAL.northlight)} Investor sync — Northlight Ventures: prep due ${hm(9, 50)}.\n`, `- [x] ${hm(9, 51)} prep sent: Investor sync — Northlight Ventures ${clockLabel(CAL.northlight)} (attendees, open asks, one question).\n`), "prep sent"),
        ],
        text: `Prep sent (#${e16.envelope.seq}): who is on the call, Elena's "term sheet after references" line from people/investors.md, the two open asks, and one question for Sam. The event is marked prepped so the poll leaves it alone; today's log has the line.`,
      },
    ],
  }).id;
  e17.envelope.runId = appendScriptedRun(store, telegram, {
    at: t(10, 6),
    user: e17.prompt,
    steps: [
      {
        thinking: "Two asks. The 1:1 is internal — I can move it myself once I know Priya is free tomorrow at the same time. The pricing goes to research with the file path and a deadline, receipt requested. Marco's Monday note about moving Priya's 1:1 is worth one dry line.",
        tools: [
          mcpCall("calendar.list_events", { calendarId: "primary", q: "1:1 Priya", timeMin: atIso(at(0, 0, 0)), timeMax: atIso(at(0, 23, 59)) }, { events: [CALENDAR_TODAY.events[2]] }),
          mcpCall("calendar.free_busy", { calendars: ["priya@lumen.example", "ada@lumen.example"], timeMin: atIso(CAL.priya + DAY_MS), timeMax: atIso(CAL.priya + DAY_MS + 50 * MINUTE_MS) }, { "priya@lumen.example": { busy: [] }, "ada@lumen.example": { busy: [] } }),
          vfsReadFile("/memory/people/marco.md", PEOPLE_MARCO),
        ],
      },
      {
        tools: [
          mcpCall("calendar.update_event", { calendarId: "primary", eventId: "evt_1on1_priya", start: atIso(CAL.priya + DAY_MS), end: atIso(CAL.priya + DAY_MS + 50 * MINUTE_MS), sendUpdates: "all" }, { id: "evt_1on1_priya", start: atIso(CAL.priya + DAY_MS), status: "confirmed", attendeesNotified: ["priya@lumen.example"] }),
          botEmit(
            {
              to: BOT.research,
              kind: "research.request",
              summary: `Competitor pricing for the Series A deck: Beacon Analytics and Orbit Insights vs Lumen — tiers, seat and usage pricing, discounts, free tier, last price change. Write to /memory/research/competitor-pricing.md by ${hm(12, 0)}.`,
              data: { deck: "Series A", competitors: ["Beacon Analytics", "Orbit Insights"], baseline: "https://lumen.example/pricing", writeTo: "/memory/research/competitor-pricing.md", dueBy: hm(12, 0) },
              reply: true,
            },
            SEQ.researchRequest,
          ),
          vfsWriteFile(`/memory/daily/${TODAY}.md`, DAILY_TODAY_MIDDAY, "1:1 moved; research asked"),
        ],
      },
      {
        tools: [messageSend(TELEGRAM, RESCHEDULE_TEXT, e17.envelope.seq ?? 0, e18.envelope.seq ?? 0)],
        text: `Moved the 1:1 to tomorrow ${clockLabel(CAL.priya)} — internal, so no yes needed; Priya's invite is updated. Research has the pricing (their #${SEQ.researchRequest}, receipt requested, due ${hm(12, 0)}). Told Ada both (#${e18.envelope.seq}), with Marco's "four this month" note.`,
      },
    ],
  }).id;
  e19.envelope.runId = appendScriptedRun(store, telegram, {
    at: t(11, 0),
    user: e19.prompt,
    steps: [
      {
        thinking: "Checklist item 4: research has been at it for 53 minutes with no receipt and the file is due by lunch. A nudge to research is cheap and not a message to Ada.",
        tools: [
          vfsReadFile(`/memory/daily/${TODAY}.md`, DAILY_TODAY_MIDDAY),
          botEmit({ to: BOT.research, kind: "research.request", summary: `Nudge on #${SEQ.researchRequest}: competitor pricing for the Series A deck is still needed by ${hm(12, 0)}.`, data: { nudge: true, of: SEQ.researchRequest } }, SEQ.researchNudge),
        ],
        text: `Nudged research (their #${SEQ.researchNudge}); nothing else on the checklist. Nothing for Ada.`,
      },
    ],
  }).id;
  e20.envelope.runId = appendScriptedRun(store, telegram, {
    at: t(11, 9),
    user: e20.prompt,
    steps: [
      {
        thinking: "The receipt for my #1. Read the file before summarising it — three lines of reading, and the two gaps, so Ada knows what is not in it.",
        tools: [vfsReadFile("/memory/research/competitor-pricing.md", RESEARCH_PRICING)],
      },
      {
        tools: [
          messageSend(TELEGRAM, RESEARCH_TEXT, null, e21.envelope.seq ?? 0),
          vfsWriteFile(`/memory/daily/${TODAY}.md`, DAILY_TODAY_MIDDAY.replace(`- [ ] ${hm(10, 8)} Ada: competitor pricing for the Series A deck by lunch → research #${SEQ.researchRequest}.\n`, `- [x] ${hm(10, 8)} Ada: competitor pricing for the Series A deck by lunch → research #${SEQ.researchRequest}; nudged ${hm(11, 0)}; landed ${hm(11, 9)} → research/competitor-pricing.md.\n`), "research landed"),
        ],
        text: `Pricing landed before lunch; sent Ada the reading and the two gaps (#${e21.envelope.seq}). Log updated.`,
      },
    ],
  }).id;
  e22.envelope.runId = appendScriptedRun(store, telegram, {
    at: t(11, 17),
    user: e22.prompt,
    steps: [
      {
        thinking: "Important mail from Elena, right after the sync. Playbook: read the whole thread, the person's file, and commitments — the cohort cut and the references are already promised for today, so this is Needs reply with everything in hand.",
        tools: [
          vfsReadFile("/skills/email-triage/SKILL.md", SKILL_EMAIL_TRIAGE),
          mcpCall("gmail.get_message", { id: ELENA_MAIL_ID, format: "full" }, { id: ELENA_MAIL_ID, threadId: ELENA_THREAD_ID, from: "Elena Marsh <elena@northlight.example>", to: "ada@lumen.example", cc: "sam@northlight.example", subject: "Re: today's sync — follow-ups", date: atIso(t(11, 16)), body: ELENA_BODY }),
          vfsReadFile("/memory/people/investors.md", PEOPLE_INVESTORS),
          vfsReadFile("/memory/commitments.md", COMMITMENTS_MORNING),
        ],
      },
      {
        tools: [
          mcpCall("gmail.create_draft", { threadId: ELENA_THREAD_ID, to: "elena@northlight.example", cc: "sam@northlight.example", subject: "Re: today's sync — follow-ups", body: ELENA_DRAFT, attachments: [{ driveFileId: "1xKq9dR2cohort", name: "cohort-retention-2025.csv" }] }, { draftId: ELENA_DRAFT_ID, threadId: ELENA_THREAD_ID, attachments: 1 }),
          messageSend(TELEGRAM, TRIAGE_TEXT, null, e23.envelope.seq ?? 0),
        ],
        text: `Needs reply, drafted (${ELENA_DRAFT_ID}): the cut attached from the data room, the three cleared references from people/investors.md, five lines in Ada's voice. Asked Ada for "send it" (#${e23.envelope.seq}); nothing is sent.`,
      },
    ],
  }).id;
  e24.envelope.runId = appendScriptedRun(store, telegram, {
    at: t(11, 24),
    user: e24.prompt,
    steps: [
      {
        thinking: "\"send it\" — the yes for the Elena draft, the only draft I asked about in this thread. Send it, mark the commitment done with the time, update investors.md and the log.",
        tools: [
          mcpCall("gmail.send", { draftId: ELENA_DRAFT_ID }, { id: "19b4d02a7c5e6b14", threadId: ELENA_THREAD_ID, labelIds: ["SENT"] }),
          vfsWriteFile("/memory/commitments.md", COMMITMENTS_AFTER_SEND, "Elena row done"),
          vfsWriteFile("/memory/people/investors.md", PEOPLE_INVESTORS_AFTER_SYNC, "today's sync and the send"),
          vfsWriteFile(`/memory/daily/${TODAY}.md`, DAILY_TODAY_MIDDAY.replace(`- [ ] ${hm(10, 8)} Ada: competitor pricing for the Series A deck by lunch → research #${SEQ.researchRequest}.\n`, `- [x] ${hm(10, 8)} Ada: competitor pricing for the Series A deck by lunch → research #${SEQ.researchRequest}; nudged ${hm(11, 0)}; landed ${hm(11, 9)} → research/competitor-pricing.md.\n- [x] ${hm(11, 17)} mail.important: Elena Marsh — post-sync follow-ups (cohort cut, references). Drafted → Ada "send it" ${hm(11, 24)} → sent ${hm(11, 25)}. commitments.md updated.\n`), "Elena sent"),
        ],
      },
      {
        tools: [messageSend(TELEGRAM, SENT_TEXT, e24.envelope.seq ?? 0, e25.envelope.seq ?? 0)],
        text: `Sent to Elena with the attachment (#${e25.envelope.seq}). commitments.md row marked done at ${hm(11, 25)}; investors.md has today's sync; the Kestrel nudge is still the one draft waiting.`,
      },
    ],
  }).id;
  e26.envelope.runId = appendScriptedRun(store, telegram, {
    at: t(13, 40),
    user: e26.prompt,
    steps: [
      {
        thinking: "A memory question. commitments.md is the source; cite it. The rota row says done in Slack — check that one line rather than trust the file blindly.",
        tools: [
          vfsReadFile("/memory/commitments.md", COMMITMENTS_AFTER_SEND),
          mcpCall("slack.search_messages", { query: "from:@ada rota in:#eng-leads after:" + daysAgoLabel(2), count: 3 }, { matches: [{ channel: "#eng-leads", user: "Ada Demo", ts: atIso(at(1, 16, 10)), text: "Rota change approved, ship it Monday." }], total: 1 }),
        ],
      },
      {
        tools: [messageSend(TELEGRAM, PROMISE_TEXT, e26.envelope.seq ?? 0, e27.envelope.seq ?? 0)],
        text: `Three commitments to Priya this week from commitments.md — the offer decision (Sep 3), the rota review (done yesterday 16:10, confirmed in #eng-leads), the Kestrel CTO intro (Sep 5) — plus the moved 1:1 (#${e27.envelope.seq}).`,
      },
    ],
  }).id;
  e28.envelope.runId = appendScriptedRun(store, telegram, {
    at: t(14, 30),
    user: e28.prompt,
    steps: [
      {
        tools: [vfsReadFile(`/memory/daily/${TODAY}.md`, DAILY_TODAY)],
        text: `Checklist: nothing past its time; Elena's reply went at ${hm(11, 25)}; research landed at ${hm(11, 9)}; the next prep (candidate debrief) is due ${hm(15, 20)} and calendar-poll has it. Nothing to say.`,
      },
    ],
  }).id;

  const state = botState({
    bot: record,
    sessions: [botSession(mainSession, "main"), botSession(telegram, "keyed"), botSession(whatsapp, "keyed")],
    recentEvents: [
      recent(e28.envelope, { inputTokens: 21_340, cachedInputTokens: 20_910 }),
      recent(e26.envelope, { inputTokens: 21_120, cachedInputTokens: 20_400 }),
      recent(e24.envelope, { inputTokens: 20_870, cachedInputTokens: 20_150 }),
      recent(e22.envelope, { inputTokens: 20_210, cachedInputTokens: 18_960 }),
      recent(e20.envelope, { inputTokens: 19_480, cachedInputTokens: 18_900 }),
      recent(e19.envelope, { inputTokens: 18_930, cachedInputTokens: 18_600 }),
      recent(e17.envelope, { inputTokens: 18_620, cachedInputTokens: 17_410 }),
      recent(e15.envelope, { inputTokens: 17_950, cachedInputTokens: 17_020 }),
    ],
    eventsProcessed: 2_146,
    appliedProfileRevision: ASSISTANT_PROFILE.revision,
    runsToday: 14,
  });
  universe.bots.set(BOT.assistant, { bot: record, triggers, events: log.events, state, descendants: [] });
}

// ---------------------------------------------------------------------------
// Research: delegated questions, answered with sub-agents
// ---------------------------------------------------------------------------

const RESEARCH_BRIEF = `Requests come from the assistant with a path under /memory/research and a due time. Split by competitor or market — one sub-agent each, three at most unless the request names more — then join, synthesise, write the file, and end with the two-line summary the receipt carries. Lumen's own pricing page (lumen.example/pricing) is the baseline column every time. A nudge on a request already in progress is a duplicate: ignore it.`;

const PRICING_REQUEST_SUMMARY = `Competitor pricing for the Series A deck: Beacon Analytics and Orbit Insights vs Lumen — tiers, seat and usage pricing, discounts, free tier, last price change. Write to /memory/research/competitor-pricing.md by ${hm(12, 0)}.`;
const BEACON_TASK = "Beacon Analytics (beacon-analytics.example): every public price — tiers, per-seat and usage components, minimums, annual discount, free tier, enterprise floor — and the date of the last price change from their changelog. Table plus source URLs; say what is not published.";
const ORBIT_TASK = "Orbit Insights (orbit-insights.example): every public price — tiers, per-seat and usage components, annual discount, trial, enterprise floor — and when the current tiers were introduced. Table plus source URLs; say what is not published.";
const BEACON_REPORT = `Beacon Analytics — public pricing (fetched ${TODAY})

| Tier | Price | Notes |
| --- | --- | --- |
| Free | $0, 3 users, forever | 10k events/mo |
| Starter | $39/user/mo, min 5 users | annual −20% |
| Professional | $79/user/mo | annual −20%; SSO |
| Enterprise | custom, "from $75k/yr" (pricing FAQ) | minimum seats not published |

Usage: events above 1M/mo at $0.50 per 1k on Starter and Professional.
Last price change: 2026-03-12 changelog — "Starter moves from $29 to $39 for new customers"; no rollback since.
Sources: beacon-analytics.example/pricing, /pricing/faq, /changelog.
Not published: enterprise minimum seats; whether the usage rate differs on Enterprise.`;
const ORBIT_REPORT = `Orbit Insights — public pricing (fetched ${TODAY})

| Tier | Price | Notes |
| --- | --- | --- |
| Growth | $45/user/mo | 30-day trial |
| Scale | $95/user/mo | introduced 2026-01 (blog: "Introducing Scale") |
| Enterprise | custom | no floor, "talk to us" |

Usage: none — seats only on every tier. Annual: −10% ("billed annually").
Sources: orbit-insights.example/pricing, /blog/introducing-scale.
Not published: any enterprise floor; any usage or overage pricing (the pricing page has no usage section at all).`;

function seedResearch(store: DemoStore, universe: UniverseState): void {
  const record = bot(universe, {
    botId: BOT.research,
    displayName: "Research",
    description: "Delegated research for the assistant: one thread per request, parallel sub-agents, sources cited, the result written under /memory/research.",
    profileId: PROFILE.researcher,
    brief: RESEARCH_BRIEF,
    runsPerDay: 40,
    breaker: { fires: 10, windowMs: 10 * MINUTE_MS },
    routedSessionCloseAfterMs: 2 * DAY_MS,
    emit: true,
    createdAtMs: ago(30 * DAY_MS),
    updatedAtMs: ago(9 * DAY_MS),
  });
  const triggers = new Map([
    ["inbox", inboxTrigger(BOT.research, [BOT.assistant], { route: { policy: "perEvent" }, deliver: { whenBusy: "queue" }, createdAtMs: ago(30 * DAY_MS), updatedAtMs: ago(9 * DAY_MS) })],
  ]);

  const log = eventLog(store, BOT.research);
  const pricingRef = { sessionId: SESSION.researchPricing, label: "competitor pricing" };
  const nudgeRef = { sessionId: SESSION.researchNudge, label: "nudge on #1" };
  const r1 = log.add({
    kind: "research.request",
    source: `bot:${BOT.assistant}`,
    at: t(10, 7),
    summary: PRICING_REQUEST_SUMMARY,
    eventId: `bot:${BOT.assistant}:${hex(`research.request:${PRICING_REQUEST_SUMMARY}`, 24)}`,
    session: pricingRef,
    sender: BOT.assistant,
    hops: 1,
    outcome: "handled",
    detail: "Two sub-agents (Beacon, Orbit) beside the Lumen baseline; /memory/research/competitor-pricing.md written; receipt to assistant",
    resolvedAfterMs: t(11, 8) - t(10, 7),
    data: { deck: "Series A", competitors: ["Beacon Analytics", "Orbit Insights"], baseline: "https://lumen.example/pricing", writeTo: "/memory/research/competitor-pricing.md", dueBy: hm(12, 0) },
  });
  const r2 = log.add({
    kind: "research.request",
    source: `bot:${BOT.assistant}`,
    at: t(11, 1),
    summary: `Nudge on #${SEQ.researchRequest}: competitor pricing for the Series A deck is still needed by ${hm(12, 0)}.`,
    eventId: `bot:${BOT.assistant}:${hex(`research.request:nudge:${SEQ.researchRequest}`, 24)}`,
    session: nudgeRef,
    sender: BOT.assistant,
    hops: 1,
    outcome: "ignored",
    detail: `Duplicate of #${SEQ.researchRequest}, in progress in its own thread (sub-agents back, file being written); the receipt for #${SEQ.researchRequest} answers it`,
    resolvedAfterMs: 9_000,
    data: { nudge: true, of: SEQ.researchRequest },
  });
  if (r1.envelope.seq !== SEQ.researchRequest || r2.envelope.seq !== SEQ.researchNudge) {
    throw new Error(`research log order drifted: #${r1.envelope.seq}, #${r2.envelope.seq}`);
  }

  const tools = [...BOT_TOOLS, EMIT_TOOL];
  const mainSession = managedSession(store, universe, {
    id: SESSION.researchMain,
    botId: BOT.research,
    displayName: "Research",
    profile: RESEARCHER_PROFILE,
    tools,
    createdAtMs: ago(30 * DAY_MS),
  });
  appendScriptedRun(store, mainSession, {
    at: ago(30 * DAY_MS) + 20_000,
    user: INTRODUCTION_PROMPT,
    steps: [
      {
        text: "I am Research: every research.request from the assistant becomes its own thread, I split it across researcher sub-agents where the parts are independent, fetch public pages, and write the answer with sources under /memory/research; the receipt carries my two-line summary back. I have web fetch and search, the memory workspace read-write, and sub-agents up to three at a time. Nothing missing — one question: should I keep a thread open for follow-ups, or is two days enough?",
      },
    ],
  });

  const pricing = managedSession(store, universe, {
    id: SESSION.researchPricing,
    botId: BOT.research,
    displayName: "Research · competitor pricing",
    profile: RESEARCHER_PROFILE,
    tools,
    createdAtMs: t(10, 7) + 2_000,
  });
  const pricingRun = appendScriptedRun(store, pricing, {
    at: t(10, 7) + 4_000,
    user: r1.prompt,
    steps: [
      {
        thinking: "Two competitors, independent — one sub-agent each, in parallel. I fetch the Lumen baseline myself meanwhile, since the brief wants it as the first column every time.",
        tools: [
          agentSpawn(PROFILE.researcher, BEACON_TASK, "promise_1"),
          agentSpawn(PROFILE.researcher, ORBIT_TASK, "promise_2"),
          webFetch("https://lumen.example/pricing", "baseline", "Team €49/user/mo · Business €89/user/mo · Enterprise from €60k/yr. Usage: workflow runs above 50k/mo at €0.004/run. Annual −15%. 14-day trial."),
        ],
      },
      {
        tools: [
          awaitPromises(
            ["promise_1", "promise_2"],
            [
              { agent: PROFILE.researcher, sessionId: SESSION.subBeacon, output: BEACON_REPORT },
              { agent: PROFILE.researcher, sessionId: SESSION.subOrbit, output: ORBIT_REPORT },
            ],
          ),
        ],
      },
      {
        thinking: "Both back. One table with Lumen first, a short reading for the deck, the sources per column, and the gaps named — Orbit's enterprise pricing and Beacon's minimum seats are not on the sites, and I will not estimate them.",
        tools: [vfsWriteFile("/memory/research/competitor-pricing.md", RESEARCH_PRICING, "three columns, sources, two gaps")],
        text: "Wrote /memory/research/competitor-pricing.md: Lumen vs Beacon Analytics vs Orbit Insights — tiers, usage pricing, discounts, free tiers, last price change, with sources per column. Two gaps named: Orbit's enterprise floor and Beacon's enterprise minimum seats.",
      },
    ],
  });
  r1.envelope.runId = pricingRun.id;

  const beacon = subagentSession(store, universe, {
    id: SESSION.subBeacon,
    displayName: "Pricing: Beacon Analytics",
    profile: RESEARCHER_PROFILE,
    parent: pricing,
    parentRunId: pricingRun.id,
    root: SESSION.researchPricing,
    depth: 1,
    limits: RESEARCH_LIMITS,
    createdAtMs: t(10, 7) + 20_000,
  });
  appendExchange(store, universe, beacon, {
    at: t(10, 7) + 22_000,
    user: BEACON_TASK,
    turn: {
      tools: [
        webFetch("https://beacon-analytics.example/pricing", "pricing page", "Free: $0, up to 3 users, 10k events/mo. Starter: $39/user/mo (minimum 5 users). Professional: $79/user/mo, SSO, audit log. Enterprise: contact sales. Save 20% with annual billing. Usage above 1M events/mo: $0.50 per 1,000 events (Starter, Professional)."),
        webFetch("https://beacon-analytics.example/pricing/faq", "pricing FAQ", "Q: Is there a minimum for Enterprise? A: Enterprise plans start from $75,000 per year. Q: Do usage rates differ on Enterprise? A: Contact sales."),
        webFetch("https://beacon-analytics.example/changelog", "changelog", "2026-03-12 — Pricing: Starter moves from $29 to $39 per user for new customers; existing customers keep their price until renewal. 2026-01-20 — Audit log on Professional. …"),
      ],
      text: BEACON_REPORT,
    },
  });
  closeAt(beacon, t(10, 31));
  const orbit = subagentSession(store, universe, {
    id: SESSION.subOrbit,
    displayName: "Pricing: Orbit Insights",
    profile: RESEARCHER_PROFILE,
    parent: pricing,
    parentRunId: pricingRun.id,
    root: SESSION.researchPricing,
    depth: 1,
    limits: RESEARCH_LIMITS,
    createdAtMs: t(10, 7) + 21_000,
  });
  appendExchange(store, universe, orbit, {
    at: t(10, 7) + 23_000,
    user: ORBIT_TASK,
    turn: {
      tools: [
        webFetch("https://orbit-insights.example/pricing", "pricing page", "Growth: $45 per user per month. Scale: $95 per user per month. Enterprise: talk to us. Billed annually: save 10%. Start a 30-day trial."),
        webFetch("https://orbit-insights.example/blog/introducing-scale", "Scale announcement", "January 8, 2026 — Introducing Scale: a new tier for teams above 50 seats with role-based access, priority support, and a dedicated success manager, at $95 per user per month."),
      ],
      text: ORBIT_REPORT,
    },
  });
  closeAt(orbit, t(10, 28));

  const nudge = managedSession(store, universe, {
    id: SESSION.researchNudge,
    botId: BOT.research,
    displayName: "Research · nudge on #1",
    profile: RESEARCHER_PROFILE,
    tools,
    createdAtMs: t(11, 1) + 1_000,
  });
  r2.envelope.runId = appendScriptedRun(store, nudge, {
    at: t(11, 1) + 2_000,
    user: r2.prompt,
    steps: [{ text: `Same ask as #${SEQ.researchRequest} — competitor pricing for the Series A deck — which is in progress in its own thread: both sub-agents are back and the file is being written. Ignoring the nudge; the receipt for #${SEQ.researchRequest} answers it.` }],
  }).id;
  closeAt(nudge, t(11, 3));

  const descendants: SessionSummaryView[] = [
    lineageChild(beacon, PROFILE.researcher, 1),
    lineageChild(orbit, PROFILE.researcher, 1),
  ];
  const state = botState({
    bot: record,
    sessions: [botSession(mainSession, "main"), botSession(pricing, "event", pricingRef.label), botSession(nudge, "event", nudgeRef.label)],
    recentEvents: [recent(r2.envelope, { inputTokens: 6_120, cachedInputTokens: 5_800 }), recent(r1.envelope, { inputTokens: 9_870, cachedInputTokens: 7_310 })],
    eventsProcessed: 37,
    appliedProfileRevision: RESEARCHER_PROFILE.revision,
    runsToday: 2,
    descendantsToday: 2,
  });
  universe.bots.set(BOT.research, { bot: record, triggers, events: log.events, state, descendants });
}

// ---------------------------------------------------------------------------
// Metrics: the Monday numbers and billing alerts
// ---------------------------------------------------------------------------

const METRICS_BRIEF = `Six numbers, same order, last week beside them: MRR, net revenue churn, new, churned, pipeline coverage, largest customer's share. Flag to the assistant (billing.alert): a paying customer above €1k/mo, or any third failed attempt. Acme Corp is 9% of MRR — anything Acme goes out at once. Trials are noise.`;

function seedMetrics(store: DemoStore, universe: UniverseState): void {
  const record = bot(universe, {
    botId: BOT.metrics,
    displayName: "Metrics",
    description: "The Monday numbers from Stripe and HubSpot, and billing alerts from Stripe webhooks, sent to the assistant.",
    profileId: PROFILE.metrics,
    brief: METRICS_BRIEF,
    runsPerDay: 30,
    breaker: { fires: 10, windowMs: 10 * MINUTE_MS },
    emit: true,
    createdAtMs: ago(27 * DAY_MS),
    updatedAtMs: ago(13 * DAY_MS),
  });
  const triggers = new Map([
    [
      "monday-metrics",
      scheduleTrigger(
        BOT.metrics,
        "monday-metrics",
        { cron: "30 7 * * 1", summary: "Monday numbers: MRR, net revenue churn, new and churned customers, Q4 pipeline coverage, largest customer's share — from Stripe and HubSpot, last week beside. Emit metrics.weekly to the assistant." },
        { route: { policy: "bot" }, deliver: { whenBusy: "queue" }, createdAtMs: ago(27 * DAY_MS), updatedAtMs: ago(13 * DAY_MS) },
      ),
    ],
    [
      "stripe-webhook",
      webhookTrigger(
        universe,
        BOT.metrics,
        "stripe-webhook",
        {
          token: STRIPE_WEBHOOK_TOKEN,
          verification: { scheme: "hmac-sha256", grantId: GRANT.stripeSigning, header: "stripe-signature", prefix: "v1=" },
          preset: null,
        },
        {
          filter: 'data.type in ["invoice.payment_failed", "customer.subscription.deleted"]',
          route: { policy: "bot" },
          deliver: { whenBusy: "queue" },
          createdAtMs: ago(27 * DAY_MS),
        },
      ),
    ],
  ]);

  const log = eventLog(store, BOT.metrics);
  const mondayAt = at(MONDAY_DAYS_AGO, 7, 30);
  const m1 = log.add({
    kind: "schedule",
    source: "schedule:monday-metrics",
    at: mondayAt,
    summary: "Monday numbers: MRR, net revenue churn, new and churned customers, Q4 pipeline coverage, largest customer's share — from Stripe and HubSpot, last week beside. Emit metrics.weekly to the assistant.",
    eventId: `schedule:monday-metrics:${atIso(mondayAt)}`,
    session: main(BOT.metrics),
    outcome: "handled",
    detail: `Digest emitted to assistant (#${SEQ.assistantMetricsWeekly} there): MRR €412k, churn 1.1%, +2/−1, coverage 3.1×, Acme 9.3%`,
    resolvedAfterMs: 50_000,
    data: { trigger: "monday-metrics", cron: "30 7 * * 1", timezone: "Europe/Berlin", scheduledAt: atIso(mondayAt) },
  });
  const stripeEvent = (type: string, id: string, atMs: number, outcome: "handled" | "ignored", detail: string, object: Record<string, unknown>): ScriptedEvent =>
    log.add({
      kind: type,
      source: "webhook:stripe-webhook",
      at: atMs,
      summary: `Webhook ${type} received on trigger stripe-webhook: ${String(object.customerName)} — ${String(object.summary)}`,
      eventId: `webhook:stripe-webhook:${id}`,
      session: main(BOT.metrics),
      outcome,
      detail,
      resolvedAfterMs: outcome === "handled" ? 35_000 : 9_000,
      data: { id, type, created: Math.floor(atMs / 1000), data: { object } },
    });
  const m2 = stripeEvent("invoice.payment_failed", "evt_1QzT8kL2m9Xp4a", at(2, 9, 10), "handled", `Flagged to assistant (#${SEQ.assistantBillingAlert} there): Acme Corp card declined, 3rd attempt, €38,400; Stripe retries ${BILLING_RETRY_DATE}`, {
    id: "in_1QzT8",
    customer: "cus_Acme01",
    customerName: "Acme Corp",
    amount_due: 3_840_000,
    currency: "eur",
    attempt_count: 3,
    next_payment_attempt: Math.floor((NOW + DAY_MS) / 1000),
    last_finalization_error: { code: "card_declined", decline_code: "insufficient_funds" },
    summary: "invoice in_1QzT8 €38,400 payment failed, attempt 3 (card_declined / insufficient_funds)",
  });
  const m3 = stripeEvent("customer.subscription.deleted", "evt_1R0aB3nQ7wYt2c", at(1, 10, 2), "ignored", "Trial workspace (Fernly Labs, €0/mo, 14-day trial) cancelled; below the flag threshold", {
    id: "sub_1R0aB3",
    customer: "cus_Fernly7",
    customerName: "Fernly Labs",
    plan: "team-trial",
    amount: 0,
    currency: "eur",
    trial_end: Math.floor(at(1, 10, 0) / 1000),
    summary: "subscription sub_1R0aB3 (team-trial, €0/mo) cancelled at trial end",
  });

  const session = managedSession(store, universe, {
    id: SESSION.metricsMain,
    botId: BOT.metrics,
    displayName: "Metrics",
    profile: METRICS_PROFILE,
    tools: [...BOT_TOOLS, EMIT_TOOL],
    createdAtMs: ago(27 * DAY_MS),
  });
  appendScriptedRun(store, session, {
    at: ago(27 * DAY_MS) + 20_000,
    user: INTRODUCTION_PROMPT,
    steps: [
      {
        text: "I am Metrics. Mondays at 07:30 I pull MRR, net revenue churn, new and churned customers, and pipeline coverage from Stripe and HubSpot and emit the six numbers to the assistant; Stripe webhooks for failed payments and cancellations reach me signed, and I flag the ones that matter. Read-only keys on both, no memory of my own beyond the assistant's MEMORY.md. One question: the threshold for a billing alert — €1k/mo, or any paying customer?",
      },
    ],
  });
  m1.envelope.runId = appendScriptedRun(store, session, {
    at: mondayAt,
    user: m1.prompt,
    steps: [
      {
        tools: [
          mcpCall("stripe.search_subscriptions", { query: "status:'active'", limit: 100, expand: ["data.items"] }, { count: 31, mrr: { value: 412_000, currency: "eur" }, previousWeek: { mrr: 398_000, count: 30 }, new: [{ customer: "Halden Logistics", delta: 6_200, kind: "expansion" }, { customer: "Marrow Health", delta: 4_100, kind: "new" }], churned: [{ customer: "Fernly Labs", delta: 0, kind: "trial" }], largest: { customer: "Acme Corp", mrr: 38_400 } }),
          mcpCall("stripe.list_invoices", { status: "paid", created: { gte: Math.floor(at(MONDAY_DAYS_AGO + 7, 0, 0) / 1000) } }, { count: 29, total: 407_900, currency: "eur", refunds: 0, netRevenueChurn: 0.011, previous: 0.014 }),
          mcpCall("hubspot.get_pipeline", { pipeline: "sales", stage: "open", closeDateWithin: "Q4" }, { deals: 14, weighted: 1_280_000, target: 410_000, coverage: 3.1, previous: 2.9 }),
        ],
      },
      {
        tools: [botEmit({ to: BOT.assistant, kind: "metrics.weekly", summary: METRICS_WEEKLY_SUMMARY, data: METRICS_WEEKLY_DATA }, SEQ.assistantMetricsWeekly)],
        text: `Week ${MONDAY_WEEK}: MRR €412k (+3.5%), net revenue churn 1.1% (1.4%), new 2, churned 1 (a trial), Q4 coverage 3.1× (2.9×), Acme 9.3% of MRR. Emitted to the assistant (their #${SEQ.assistantMetricsWeekly}).`,
      },
    ],
  }).id;
  m2.envelope.runId = appendScriptedRun(store, session, {
    at: at(2, 9, 10),
    user: m2.prompt,
    steps: [
      {
        thinking: "Third attempt on the largest customer. Confirm the invoice and the customer before flagging, then tell the assistant what Stripe does next.",
        tools: [
          mcpCall("stripe.retrieve_invoice", { id: "in_1QzT8" }, { id: "in_1QzT8", customer: "cus_Acme01", amount_due: 3_840_000, currency: "eur", status: "open", attempt_count: 3, next_payment_attempt: Math.floor((NOW + DAY_MS) / 1000), collection_method: "charge_automatically" }),
          mcpCall("stripe.retrieve_customer", { id: "cus_Acme01" }, { id: "cus_Acme01", name: "Acme Corp", email: "ap@acme.example", mrr: 38_400, since: "2024-11-03", delinquent: false }),
        ],
      },
      {
        tools: [botEmit({ to: BOT.assistant, kind: "billing.alert", summary: BILLING_ALERT_SUMMARY, data: BILLING_ALERT_DATA }, SEQ.assistantBillingAlert)],
        text: `Acme Corp, €38,400, third failed attempt, one retry left on ${BILLING_RETRY_DATE} before past due. Flagged to the assistant (their #${SEQ.assistantBillingAlert}).`,
      },
    ],
  }).id;
  m3.envelope.runId = appendScriptedRun(store, session, {
    at: at(1, 10, 2),
    user: m3.prompt,
    steps: [
      {
        tools: [mcpCall("stripe.retrieve_subscription", { id: "sub_1R0aB3" }, { id: "sub_1R0aB3", customer: "cus_Fernly7", plan: "team-trial", amount: 0, currency: "eur", status: "canceled", trial_end: Math.floor(at(1, 10, 0) / 1000), cancellation_details: { reason: "trial_expired" } })],
        text: "Fernly Labs: a 14-day trial that ended without converting, €0/mo. Below the threshold; noise. Ignoring.",
      },
    ],
  }).id;

  const state = botState({
    bot: record,
    sessions: [botSession(session, "main")],
    recentEvents: [recent(m3.envelope, { inputTokens: 5_410, cachedInputTokens: 5_020 }), recent(m2.envelope, { inputTokens: 6_230, cachedInputTokens: 5_600 }), recent(m1.envelope, { inputTokens: 7_940, cachedInputTokens: 6_100 })],
    eventsProcessed: 41,
    appliedProfileRevision: METRICS_PROFILE.revision,
    runsToday: 0,
  });
  universe.bots.set(BOT.metrics, { bot: record, triggers, events: log.events, state, descendants: [] });
}

// ---------------------------------------------------------------------------
// Interactive sessions
// ---------------------------------------------------------------------------

const BOARD_DRAFT = `## Q3 update — draft sections (metrics, commitments, asks)

### Metrics
- MRR €412k at the ${MONDAY} digest, up from €398k the week before; net revenue churn 1.1% (1.4%). 31 paying customers; Acme Corp 9.3% of MRR.¹
- Q4 pipeline coverage 3.1× against the €410k target.¹
- Runway 19 months at current burn.²

### Commitments to the board from Q2
| Commitment | Status | Source |
| --- | --- | --- |
| Close the Series A lead by end of Q3 | Northlight IC on Sep 8 if the cohort cut and references land (sent ${TODAY}) | commitments.md, people/investors.md |
| Hire the Staff engineer | offer out; decision by Sep 3 | commitments.md |
| Ship the platform migration plan | draft mid-September; not costed yet, no dates promised | people/priya.md |

### Asks
- Second-close introductions: Kestrel is slow; one warm intro to a second lead would shorten the raise.
- A board member on the Acme 2-year term with a price hold (finance models it by Sep 2).³

¹ Monday metrics digest, ${MONDAY}. ² Finance memo Aug 20 (MEMORY.md). ³ Acme renewal call, ${YESTERDAY} (daily log).

**Two questions before I go further**
1. The week ${REVIEW_WEEK} review says the pipeline coverage sheet for Northlight slipped because "the CRM export was wrong twice" — is 3.1× from the corrected export, or should the board see a range?
2. Priya asked that no migration dates reach the board before the costing — do you want the migration section left out entirely, or one line saying "plan in September"?`;

const MARCO_AGENDA = `## 1:1 with Marco — ${MONDAY} 11:00

**His threads (people/marco.md)**
1. Board pack: he has the narrative outline; your metrics section is due Sep 5 — confirm you will use the ${MONDAY} digest numbers and tell him the migration section is a one-liner until Priya's costing lands.
2. AE hiring: two finalists, panel next week; he wants you on the final interview — give him two slots.
3. Offsite Oct 8–9: the venue shortlist is in Notion, which is not connected — ask him to paste the top three into Slack.

**Your threads**
4. Acme renewal call ${YESTERDAY}: the 2-year term with a price hold — he models it by Sep 2 (commitments.md). Ask what discount range finance is comfortable with before you answer Dana.
5. Northlight: IC on Sep 8; the coverage sheet is his — is the corrected CRM export done?

**One thing he will raise**
- Priya's 1:1 has moved three times this month (his note from ${MONDAY} in people/marco.md). Have an answer.

Not on the list: the Q3 date for the board deck template (he sends it this week); the Kestrel nudge (yours, not his).`;

const HIRING_TABLE = `Hiring pipeline this week (Notion is not reachable — its connection needs re-auth on the Integrations page — so this is from mail, calendar, and HubSpot):

| Role | Stage | Candidate | Next step | When | Owner |
| --- | --- | --- | --- | --- | --- |
| Staff engineer | offer out | Jonas Weber | decision on band and start date | by Sep 3 (his other deadline Sep 4) | Ada |
| Staff engineer | final debrief | Mira Sato | debrief with Priya and Marco | today ${clockLabel(CAL.debrief)} | Priya |
| Account executive | panel | Tomas Herrera | panel interview | next Tuesday 10:00 | Marco |
| Account executive | panel | Aisha Bello | panel interview | next Wednesday 14:00 | Marco |
| Account executive | sourcing | — | Marco wants you on both finals | slots needed | Ada |

Sources: calendar (the debrief and the two panels), mail from Priya ${YESTERDAY} (comp sheet, Jonas) and from Marco ${daysAgoLabel(2)} (finalists), HubSpot's "AE hire" deal Marco keeps as a pipeline (stage: panel). What I could not check: the Notion scorecards, and whether Mira has a competing offer — Priya's mail does not say.`;

function seedInteractiveSessions(store: DemoStore, universe: UniverseState): void {
  const board = newSession(store, universe, {
    id: SESSION.board,
    displayName: "Board update — Q3 draft",
    config: structuredClone(ASSISTANT_CONFIG),
    instructions: ASSISTANT_INSTRUCTIONS,
    activeEnvironmentId: ENV_MAC_MINI,
    createdAtMs: at(1, 11, 40),
  });
  appendScriptedRun(store, board, {
    at: at(1, 11, 40),
    user: "Draft my section of the Q3 board update: metrics, where we are on the commitments I made in Q2, and the asks. Use what's in memory; ask me what you can't answer.",
    steps: [
      {
        thinking: "Every number needs a source: the Monday digest for MRR and churn, the finance memo for runway, commitments.md for the Q2 promises, the week's review for what slipped. Write the sections, footnote them, and leave questions where memory does not answer.",
        tools: [
          vfsReadFile("/memory/commitments.md", COMMITMENTS_MORNING),
          vfsReadFile(`/briefs/${REVIEW_FILE}`, BRIEF_WEEK_REVIEW),
          vfsReadFile("/memory/MEMORY.md", MEMORY_CURRENT),
          vfsReadFile("/memory/people/priya.md", PEOPLE_PRIYA),
        ],
        text: BOARD_DRAFT,
      },
    ],
  });
  appendScriptedRun(store, board, {
    at: at(1, 11, 58),
    user: "3.1 is from the corrected export — say so. Leave the migration as one line, 'plan in September'. Save it for Marco.",
    steps: [
      {
        tools: [vfsWriteFile(`/briefs/${TODAY}-board-q3-metrics-draft.md`, `${BOARD_DRAFT.split("\n\n**Two questions")[0] ?? BOARD_DRAFT}\n\n_Coverage 3.1× is from the corrected CRM export (${MONDAY}). Platform migration: plan in September._\n`, "for Marco, Sep 5")],
        text: `Saved to /briefs/${TODAY}-board-q3-metrics-draft.md with both answers folded in: the coverage line names the corrected export, and the migration is the one line. commitments.md still shows the section as open until you send it to Marco — say the word and I draft that mail.`,
      },
    ],
  });

  const marco = newSession(store, universe, {
    id: SESSION.marco,
    displayName: "Prep: 1:1 with Marco",
    config: structuredClone(ASSISTANT_CONFIG),
    instructions: ASSISTANT_INSTRUCTIONS,
    activeEnvironmentId: ENV_MAC_MINI,
    createdAtMs: at(MONDAY_DAYS_AGO, 10, 20),
  });
  appendScriptedRun(store, marco, {
    at: at(MONDAY_DAYS_AGO, 10, 20),
    user: "Prep me for the 11:00 with Marco. His threads, mine, and whatever he's going to bring up.",
    steps: [
      {
        thinking: "Marco's file first, then commitments for what I owe him and he owes me, then the last two daily logs for anything he said in standup that has not made it into his file yet.",
        tools: [
          vfsReadFile("/memory/people/marco.md", PEOPLE_MARCO),
          vfsReadFile("/memory/commitments.md", COMMITMENTS_MORNING),
          vfsReadFile(`/memory/daily/${YESTERDAY}.md`, DAILY_YESTERDAY),
          mcpCall("slack.search_messages", { query: "from:@marco offsite venue in:#leadership", count: 3 }, { matches: [{ channel: "#leadership", user: "Marco Ruiz", ts: agoIso(6 * DAY_MS), text: "Offsite venue shortlist is in Notion under Ops → Offsite 2026; three options, all Oct 8–9." }], total: 1 }),
        ],
        text: MARCO_AGENDA,
      },
    ],
  });

  const hiring = newSession(store, universe, {
    id: SESSION.hiring,
    displayName: "Hiring pipeline this week",
    config: structuredClone(HIRING_CONFIG),
    instructions: ASSISTANT_INSTRUCTIONS,
    activeEnvironmentId: ENV_MAC_MINI,
    createdAtMs: ago(50 * MINUTE_MS),
  });
  appendScriptedRun(store, hiring, {
    at: ago(50 * MINUTE_MS),
    user: "Where's hiring this week? Both roles, one table.",
    steps: [
      {
        thinking: "The hiring board is in Notion. Try it; if the connection is still broken, mail and calendar have the interviews and HubSpot has Marco's AE pipeline.",
        tools: [
          mcpFailure("notion.search", { query: "Hiring pipeline", filter: { property: "object", value: "database" } }, "401 Unauthorized: the Notion workspace token was rejected (refresh failed 5 days ago). Reconnect the notion server on the Integrations page."),
        ],
      },
      {
        tools: [
          mcpCall("gmail.search", { q: "(from:priya@lumen.example OR from:marco@lumen.example) (candidate OR offer OR panel OR finalist) newer_than:7d", maxResults: 10 }, { messages: [{ id: "19b4a0", from: "Priya Natarajan", subject: "Jonas Weber — comp sheet", date: atIso(at(1, 14, 52)), snippet: "Band and start date options attached; his other offer expires Sep 4." }, { id: "19b3c4", from: "Marco Ruiz", subject: "AE finalists", date: agoIso(2 * DAY_MS + 3 * HOUR_MS), snippet: "Tomas Herrera and Aisha Bello to panel next week; want you on both finals." }, { id: "19b3d9", from: "Priya Natarajan", subject: "Mira Sato — final debrief", date: agoIso(2 * DAY_MS + 5 * HOUR_MS), snippet: "Debrief booked for the 25-min slot; scorecards in Notion." }], resultSizeEstimate: 3 }),
          mcpCall("calendar.list_events", { calendarId: "primary", q: "interview OR debrief OR panel", timeMin: atIso(at(0, 0, 0)), timeMax: atIso(NOW + 7 * DAY_MS), singleEvents: true }, { events: [CALENDAR_TODAY.events[3], { id: "evt_panel_herrera", summary: "AE panel — Tomas Herrera", start: "next Tuesday 10:00", attendees: ["marco@lumen.example", "ada@lumen.example"] }, { id: "evt_panel_bello", summary: "AE panel — Aisha Bello", start: "next Wednesday 14:00", attendees: ["marco@lumen.example", "ada@lumen.example"] }] }),
          mcpCall("hubspot.search_deals", { pipeline: "internal-hiring", query: "AE" }, { deals: [{ name: "AE hire — Q3", stage: "panel", owner: "Marco Ruiz", candidates: 2, updated: agoIso(2 * DAY_MS) }] }),
        ],
        text: HIRING_TABLE,
      },
    ],
  });
}

// ---------------------------------------------------------------------------
// Live responder
// ---------------------------------------------------------------------------

const RESPONDER_INBOX = {
  resultSizeEstimate: 4,
  messages: [
    { id: "19b4d4a1", from: "Elena Marsh <elena@northlight.example>", subject: "Re: today's sync — follow-ups", labels: ["IMPORTANT", "INBOX"], snippet: "Got both, thank you — Sam will confirm the IC slot." },
    { id: "19b4d2c7", from: "Dana Whitfield <dana@acme.example>", subject: "2-year term", labels: ["IMPORTANT", "INBOX", "UNREAD"], snippet: "Following up on Thursday's call: can you confirm the price hold in writing by Friday?" },
    { id: "19b4d1b0", from: "Marco Ruiz <marco@lumen.example>", subject: "New board deck template", labels: ["INBOX", "UNREAD"], snippet: "Attached. Metrics section is slides 4–6." },
  ],
};

function briefTurn(turn: number): DemoTurn {
  if (turn > 1) {
    return {
      text: `Still the same plate, one change: Dana (Acme) wants the price hold confirmed in writing by Friday — that is a yes-or-no for you, and Marco is modelling the term by Sep 2, so I would wait for his number. Say *draft it* and I write the holding reply.`,
    };
  }
  return {
    thinking: "What is on her plate: the calendar for the rest of today, the inbox since the last poll, and the open commitments — sourced, six lines.",
    tools: [
      mcpCall("calendar.list_events", { calendarId: "primary", timeMin: agoIso(0), timeMax: atIso(at(0, 23, 59)), singleEvents: true }, { events: [CALENDAR_TODAY.events[3]] }),
      mcpCall("gmail.search", { q: "is:unread newer_than:1d -category:promotions", maxResults: 10 }, RESPONDER_INBOX),
      vfsReadFile("/memory/commitments.md", COMMITMENTS_AFTER_SEND),
    ],
    text: `On your plate:
• ${clockLabel(CAL.debrief)} candidate debrief — Staff engineer (Priya, Marco); prep comes at ${hm(15, 20)}.
• Inbox: Dana (Acme) wants the 2-year price hold confirmed in writing by Friday — needs you; Marco sent the new board template — FYI. Elena confirmed she has the cut and the references.
• Open (commitments.md): Staff offer decision by Sep 3; metrics section to Marco by Sep 5; Kestrel nudge still drafted, not sent.
• Nothing else past its time. Say *draft it* for Dana, or *send it* for Tom.`,
  };
}

function inboxTurn(turn: number): DemoTurn {
  if (turn > 1) {
    return {
      tools: [mcpCall("gmail.create_draft", { threadId: "19b4d2c7", to: "dana@acme.example", subject: "Re: 2-year term", body: "Dana — yes to the 2-year term in principle; finance is confirming the price-hold mechanics and I will have it in writing by Friday. Ada" }, { draftId: "r-2874601190", threadId: "19b4d2c7" })],
      text: "Drafted the holding reply to Dana: yes in principle, the written confirmation by Friday once finance has the mechanics, signed Ada. Say *send it* and I'll send it; otherwise it waits.",
    };
  }
  return {
    thinking: "Inbox triage per the playbook: read the thread, check the person and the commitments, draft — never send.",
    tools: [
      vfsReadFile("/skills/email-triage/SKILL.md", SKILL_EMAIL_TRIAGE),
      mcpCall("gmail.search", { q: "is:unread label:IMPORTANT newer_than:1d", maxResults: 10 }, RESPONDER_INBOX),
      mcpCall("gmail.get_message", { id: "19b4d2c7", format: "full" }, { id: "19b4d2c7", from: "Dana Whitfield <dana@acme.example>", subject: "2-year term", body: "Ada — following up on Thursday's call. We can commit to two years if the price is held for the term. Can you confirm that in writing by Friday? Our procurement needs it for the renewal packet. Dana" }),
      vfsReadFile("/memory/commitments.md", COMMITMENTS_AFTER_SEND),
    ],
    text: `Triage:
• **Needs reply** — Dana (Acme): confirm the 2-year price hold in writing by Friday. Marco is modelling the term by Sep 2 (commitments.md), so the honest reply is a holding one. Draft ready: "yes in principle, written confirmation by Friday once finance has the mechanics", signed Ada.
• **FYI** — Marco: new board deck template, metrics on slides 4–6. Elena: has the cut and the references; Sam confirms the IC slot.
• **Archive** — nothing new.
Say *send it* and I'll send the Dana reply; I won't send anything until you do.`,
  };
}

function sendTurn(turn: number): DemoTurn {
  if (turn > 1) {
    return { text: "Nothing is waiting for a send right now — the last draft went out. Tell me who to write to and I draft it; you say *send it*." };
  }
  return {
    tools: [
      mcpCall("gmail.send", { draftId: "r-2874601190" }, { id: "19b4d6e2", threadId: "19b4d2c7", labelIds: ["SENT"] }),
      vfsWriteFile(`/memory/daily/${TODAY}.md`, `${DAILY_TODAY}- ${clockLabel(NOW)} Dana (Acme): holding reply on the price hold sent after "send it".\n`, "Dana reply sent"),
    ],
    text: "Sent to Dana. Logged in today's file; nothing in commitments.md changes until finance gives you the number — Marco, Sep 2.",
  };
}

function prepTurn(turn: number): DemoTurn {
  if (turn > 1) {
    return {
      text: `Short version for the debrief: Mira Sato is the second Staff finalist; the open question is whether you make a second offer or keep Jonas Weber's as the only one — his other deadline is Sep 4 (people/priya.md). One question for the room: "If we could only hire one, which, and why?"`,
    };
  }
  return {
    thinking: "Meeting prep for the next meeting on the calendar — the candidate debrief. Attendees are internal; the candidate context is in Priya's file and yesterday's mail.",
    tools: [
      vfsReadFile("/skills/meeting-prep/SKILL.md", SKILL_MEETING_PREP),
      vfsReadFile("/memory/people/priya.md", PEOPLE_PRIYA),
      mcpCall("gmail.search", { q: "from:priya@lumen.example Mira Sato newer_than:7d", maxResults: 3 }, { messages: [{ id: "19b3d9", subject: "Mira Sato — final debrief", snippet: "Debrief booked for the 25-min slot; scorecards in Notion. Strong on distributed systems, lighter on people leadership than Jonas." }], resultSizeEstimate: 1 }),
    ],
    text: `Candidate debrief — Staff engineer, ${clockLabel(CAL.debrief)} (25 min), with Priya and Marco.
• Mira Sato, second finalist: strong on distributed systems, lighter on people leadership than Jonas Weber (Priya's mail ${daysAgoLabel(2)}). Scorecards are in Notion — not reachable until it is reconnected.
• Open thread: the Jonas Weber offer decision is yours by Sep 3; his other deadline is Sep 4 (people/priya.md, commitments.md).
• Priya's standing ask: decisions with reasons, in writing.
• Opening question: "If we could only hire one of the two, which — and what would we lose?"`,
  };
}

function rescheduleTurn(turn: number): DemoTurn {
  if (turn > 1) {
    return { text: "Tell me which meeting and when — internal ones I move myself and say so; anything with an investor, a customer, or a candidate I hold as tentative and ask you first." };
  }
  return {
    thinking: "A move. Which meeting, is it internal, and is the other side free — then move it and say so; an outsider would need a yes.",
    tools: [
      mcpCall("calendar.list_events", { calendarId: "primary", timeMin: agoIso(0), timeMax: atIso(NOW + 2 * DAY_MS), singleEvents: true }, { events: [CALENDAR_TODAY.events[3], { ...CALENDAR_TODAY.events[2], start: atIso(CAL.priya + DAY_MS), end: atIso(CAL.priya + DAY_MS + 50 * MINUTE_MS) }] }),
      mcpCall("calendar.free_busy", { calendars: ["priya@lumen.example", "marco@lumen.example"], timeMin: atIso(CAL.debrief + DAY_MS), timeMax: atIso(CAL.debrief + DAY_MS + 25 * MINUTE_MS) }, { "priya@lumen.example": { busy: [] }, "marco@lumen.example": { busy: [{ start: atIso(CAL.debrief + DAY_MS), end: atIso(CAL.debrief + DAY_MS + 30 * MINUTE_MS) }] } }),
    ],
    text: `The next movable meeting is the ${clockLabel(CAL.debrief)} candidate debrief. Tomorrow at the same time Priya is free and Marco is not (busy until ${clockLabel(CAL.debrief + 30 * MINUTE_MS)}); ${clockLabel(CAL.debrief + 30 * MINUTE_MS)} tomorrow works for both. It is internal, so I can move it — say *yes* and I update the invite, or give me another slot. (Priya's 1:1 is already tomorrow ${clockLabel(CAL.priya)}.)`,
  };
}

function travelTurn(turn: number): DemoTurn {
  if (turn > 1) {
    return { text: "Reply A or B and I book it — train, room, calendar events, and a line in MEMORY.md. Nothing is booked until you pick." };
  }
  return {
    thinking: "Travel playbook: venue and dates first, then trains on the Mac mini and two hotels within walking distance under the limit.",
    tools: [
      vfsReadFile("/skills/travel/SKILL.md", SKILL_TRAVEL),
      vfsReadFile("/memory/MEMORY.md", MEMORY_CURRENT),
      mac(["python3", "trains.py", "München Hbf", "Berlin Hbf", "2026-09-15", "--arrive-by", "12:30"], JSON.stringify({ outbound: [{ train: "ICE 1006", dep: "08:07", arr: "12:22", price: 89 }, { train: "ICE 1004", dep: "06:07", arr: "10:22", price: 79 }] }, null, 2)),
      webFetch("https://hotels.example/search?near=Molkenmarkt+2%2C+Berlin&checkin=2026-09-15&nights=1&max=220", "aggregator search", "Hotel Luc (0.3 km, from €189, breakfast), Wilde Aparthotel (0.5 km, from €164), Hotel Nikolai (0.4 km, from €212)…"),
    ],
    text: `Berlin Sep 15–16 is already booked (Hotel Luc, LUC-48213; train DB-7K3QP2 — MEMORY.md). If this is a different trip, two options the way I would do it:
A — ICE 08:07 → 12:22, Hotel Luc, 4-min walk, €189 with breakfast.
B — ICE 06:07 → 10:22 (earlier, €10 less), Wilde Aparthotel, 6-min walk, €164, no breakfast.
Reply A or B and I book; tell me the dates and the venue if it is not the summit.`,
  };
}

function promiseTurn(turn: number): DemoTurn {
  if (turn > 1) {
    return { text: "Everything I hold is in /memory/commitments.md — seven rows this week, three open past today: the Staff offer (Sep 3), the metrics section to Marco (Sep 5), the Kestrel intro for Priya (Sep 5). Name a person and I give you their rows." };
  }
  return {
    thinking: "A memory question: commitments.md is the source, cite it, and check the one row that claims something happened in Slack.",
    tools: [
      vfsReadFile("/memory/commitments.md", COMMITMENTS_AFTER_SEND),
      mcpCall("gmail.search", { q: "in:sent newer_than:3d -has:replies", maxResults: 5 }, SENT_NO_REPLY),
    ],
    text: `From commitments.md:
• **Open:** Staff offer decision (Priya, Sep 3) · metrics section to Marco (Sep 5) · Kestrel CTO intro for Priya (Sep 5, waiting on Tom) · Acme 2-year term modelled by Marco (Sep 2, his).
• **Done this week:** rota review (Priya, ${YESTERDAY}) · cohort cut and references to Elena (${TODAY} ${hm(11, 25)}).
• **Waiting on a reply:** Tom Adeyemi, 72 h — the nudge is drafted; investors.md says never twice, so it is your call. Say *send it*.`,
  };
}

function researchTurn(turn: number): DemoTurn {
  if (turn > 1) {
    return { text: "Say what to research and where to put it, and I hand it to the research bot with a receipt requested — or, for something small, I spawn a researcher sub-agent here and wait for it. Either way you get sources and the gaps named." };
  }
  return {
    thinking: "Delegation. A one-off question fits a sub-agent in this session; a deliverable for the deck goes to the research bot with a path and a due time. Show both.",
    tools: [
      agentSpawn(PROFILE.researcher, "Northlight Ventures: their last three B2B software investments with round, date, and lead status, from public announcements only. Three lines, sourced.", "promise_1"),
      botEmit({ to: BOT.research, kind: "research.request", summary: "Second-product usage benchmarks: what share of revenue comparable B2B workflow companies attribute to a second product 18 months after launch; public sources. Write to /memory/research/second-product-benchmarks.md by tomorrow 09:00.", data: { writeTo: "/memory/research/second-product-benchmarks.md", dueBy: "tomorrow 09:00" }, reply: true }, SEQ.researchNudge + 1),
    ],
    text: `Two things started:
• A researcher sub-agent in this session on Northlight's last three B2B investments (promise_1) — I'll fold the answer in here when it returns.
• The research bot has the second-product benchmarks (their #${SEQ.researchNudge + 1}, receipt requested), writing to /memory/research/second-product-benchmarks.md by tomorrow 09:00 — the same path the competitor pricing took this morning.
I'll message you when either lands.`,
  };
}

function defaultTurn(turn: number): DemoTurn {
  if (turn > 1) {
    return { text: "Still here. Ask for *the brief*, *the inbox*, *prep* for a meeting, to *move* one, to *book* travel, what you *promised* someone, or *research* something — or just say what happened and I'll put it in memory." };
  }
  return {
    text: `I'm Ada's assistant, working from the memory under /memory and the playbooks under /skills. Three things I can do right now:
1. **What's on my plate** — the rest of today from the calendar, the inbox since the last poll, and every open commitment, with the file each claim came from.
2. **Triage the inbox** — Urgent / Needs reply / FYI / Archive, with replies drafted in your voice; nothing is sent until you say *send it*.
3. **Prep the next meeting** — attendees, last interactions, open threads, one opening question, about 40 minutes before.
Or: move a meeting, book travel (two options, you pick), ask what you promised someone, or hand something to research.`,
  };
}

const responder: DemoResponder = (input, context) => {
  const text = input.toLowerCase();
  if (/\bsend it\b/.test(text)) return sendTurn(context.turn);
  if (/\bbrief\b|\btoday\b|\bplate\b/.test(text)) return briefTurn(context.turn);
  if (/\bemail\b|\binbox\b|\breply\b|\bdraft\b|\bmail\b/.test(text)) return inboxTurn(context.turn);
  if (/\bmeeting\b|\bprep\b|\b1:1\b|\bdebrief\b/.test(text)) return prepTurn(context.turn);
  if (/\bmove\b|\breschedule\b/.test(text)) return rescheduleTurn(context.turn);
  if (/\bbook\b|\btravel\b|\bflight\b|\bhotel\b|\btrain\b/.test(text)) return travelTurn(context.turn);
  if (/\bpromise|\bremind|\bfollow/.test(text)) return promiseTurn(context.turn);
  if (/\bresearch\b/.test(text)) return researchTurn(context.turn);
  return defaultTurn(context.turn);
};

// ---------------------------------------------------------------------------
// Seed
// ---------------------------------------------------------------------------

/// Ada's channel accounts and the pairing rows binding her two direct
/// conversations to the assistant's chat triggers.
function seedChannels(universe: UniverseState): void {
  channelAccount(universe, {
    accountId: TELEGRAM_ADA_ACCOUNT_ID,
    provider: "telegram",
    providerAccountId: "ada_assistant_bot",
    displayName: "Ada's assistant (Telegram)",
    credentialGrantId: "grant-telegram-bot-token",
    createdAtMs: Date.parse("2026-07-25T18:30:00.000Z"),
    updatedAtMs: Date.parse("2026-08-20T07:00:00.000Z"),
  });
  channelAccount(universe, {
    accountId: WHATSAPP_ADA_ACCOUNT_ID,
    provider: "whatsapp",
    providerAccountId: "+4917612345678",
    displayName: "Ada's assistant (WhatsApp)",
    settings: { printQr: false },
    createdAtMs: Date.parse("2026-08-02T09:15:00.000Z"),
    updatedAtMs: Date.parse("2026-08-24T19:40:00.000Z"),
  });
  channelPairing(universe, {
    accountId: TELEGRAM_ADA_ACCOUNT_ID,
    botId: BOT.assistant,
    triggerId: "telegram",
    chatId: TELEGRAM.chatId,
    pairedVia: "code",
    pairedAtMs: ago(33 * DAY_MS),
  });
  channelPairing(universe, {
    accountId: WHATSAPP_ADA_ACCOUNT_ID,
    botId: BOT.assistant,
    triggerId: "whatsapp",
    chatId: WHATSAPP.chatId,
    pairedVia: "code",
    pairedAtMs: ago(25 * DAY_MS),
  });
}

export function seedPersonalAssistant(store: DemoStore): void {
  const universe = store.addUniverse({
    id: PERSONAL_ASSISTANT_UNIVERSE_ID,
    slug: PERSONAL_ASSISTANT_SLUG,
    name: "Personal Assistant",
    lightspeedUniverseId: LIGHTSPEED_UNIVERSE_ID,
    role: "owner",
    createdAt: agoIso(5 * 7 * DAY_MS),
    responder,
  });
  seedMembers(store, universe);
  seedProfiles(universe);
  seedWorkspaces(store, universe);
  seedEnvironments(universe);
  seedIntegrations(universe);
  seedChannels(universe);
  seedAssistant(store, universe);
  seedResearch(store, universe);
  seedMetrics(store, universe);
  seedInteractiveSessions(store, universe);
}
