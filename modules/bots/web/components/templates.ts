/// Starting points for the creation wizard: a job description, the triggers
/// that fit it, and the capabilities it needs. Templates only prefill the
/// form — everything stays editable before Create, and after.
export type TemplateTrigger =
  | { kind: "schedule"; name: string; cron: string; timezone: string; summary: string }
  | { kind: "webhook"; name: string; preset: "github" | null; perKey: boolean; filter?: string }
  | { kind: "bot"; name: string };

export interface BotTemplate {
  id: string;
  name: string;
  /** A first name for the bot, and the id derived from it; null leaves both to the person. */
  suggestedName: string | null;
  description: string;
  brief: string;
  /** Sparse `SessionConfig.features` grants to start from. */
  features: Record<string, unknown>;
  triggers: TemplateTrigger[];
  runsPerDay: number | null;
  selfConfig: boolean;
  emit: boolean;
}

export const BOT_TEMPLATES: BotTemplate[] = [
  {
    id: "blank",
    name: "Blank",
    suggestedName: null,
    description: "Start from nothing: you write the brief and pick the triggers.",
    brief: "",
    features: {},
    triggers: [],
    runsPerDay: 50,
    selfConfig: true,
    emit: false,
  },
  {
    id: "pr-reviewer",
    name: "Pull-request reviewer",
    suggestedName: "Reviewer",
    description: "Reviews every pull request in its own thread and sends a weekday digest.",
    brief:
      "You review pull requests for the team.\n\nOn a new or updated pull request: read the diff, look for real defects and unclear code, and summarise what you found — one point per real issue, no nitpicks. Say clearly when a change looks good.\n\nEvery weekday morning, send a short digest: pull requests waiting for review, anything blocked for more than two days, and what merged yesterday.",
    features: { web: { fetch: {} } },
    triggers: [
      { kind: "webhook", name: "github-prs", preset: "github", perKey: true },
      {
        kind: "schedule",
        name: "morning-digest",
        cron: "0 9 * * 1-5",
        timezone: "UTC",
        summary: "Send the morning digest of open pull requests: waiting for review, blocked, merged yesterday.",
      },
    ],
    runsPerDay: 100,
    selfConfig: true,
    emit: false,
  },
  {
    id: "daily-digest",
    name: "Daily digest",
    suggestedName: "Digest",
    description: "Wakes once a day, gathers what matters, and writes a summary.",
    brief:
      "Every morning you prepare a digest for the team. Gather what changed since yesterday from the sources you can reach, keep it to the five most important items, and write it so someone can read it in a minute. Link to sources.",
    features: { web: { fetch: {}, search: {} } },
    triggers: [
      {
        kind: "schedule",
        name: "daily",
        cron: "0 8 * * *",
        timezone: "UTC",
        summary: "Prepare and send today's digest.",
      },
    ],
    runsPerDay: 10,
    selfConfig: true,
    emit: false,
  },
  {
    id: "chat-assistant",
    name: "Chat assistant",
    suggestedName: "Assistant",
    description: "Answers questions when someone starts a conversation.",
    brief:
      "You are the team's assistant. Answer directly and briefly, ask when something is ambiguous, and remember what each person told you earlier in the conversation. Never share one conversation's details in another.",
    features: { web: { fetch: {}, search: {} } },
    triggers: [],
    runsPerDay: null,
    selfConfig: false,
    emit: false,
  },
  {
    id: "on-call",
    name: "On-call responder",
    suggestedName: "On-call",
    description: "Takes alerts from a webhook and from other bots, triages, and reports.",
    brief:
      "You are the on-call responder. When an alert arrives, work out what is affected and how badly, check whether it is already known, and write a short status: what happened, impact, next step. Escalate to a person when you are unsure or when customer data may be involved.",
    features: { web: { fetch: {} } },
    triggers: [
      { kind: "webhook", name: "alerts", preset: null, perKey: false },
      { kind: "bot", name: "inbox" },
    ],
    runsPerDay: 200,
    selfConfig: true,
    emit: true,
  },
];
