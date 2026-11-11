import { useState, type ReactNode } from "react";
import {
  ActivityIcon,
  BotIcon,
  ChannelIcon,
  EnvironmentIcon,
  InboxIcon,
  ManagedIcon,
  ModelIcon,
  PauseIcon,
  ScheduleIcon,
  SessionCloseIcon,
  SessionIcon,
  SharedIcon,
  SubAgentIcon,
  TimerIcon,
  ToolsIcon,
  WebIcon,
  WebhookIcon,
  WorkspaceIcon,
} from "@/icons";
import { ActivityDot } from "@/components/activity-dot";
import { StatusDot } from "@/components/bot/status";
import { DetailHeader } from "@/components/detail-header";
import { ListPaneBody, ListPaneHeader, ListRow, ListRowLine } from "@/components/list-pane";
import { MetaToolbar, metaItemsFor, type MetaItem, type MetaPlace } from "@/components/meta-toolbar";

/// The places of a page: its header and the strip under it.
const PAGE: MetaPlace[] = ["header", "strip"];
const PLACES: MetaPlace[] = ["list", "header", "strip"];
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { cn } from "@/lib/utils";

/// Examples of the meta toolbar for the UI overview: one list of items per
/// session and per bot, drawn in its list row, its page header and the
/// strip under the header, at widths you choose.

/// A details popover's body, shaped like the session resource chips'.
function Details({ title, detail, rows }: { title: string; detail: string; rows: Array<{ primary: string; secondary?: string; tone?: string }> }) {
  return (
    <div className="grid gap-0.5">
      <div className="px-2 pt-1 pb-1.5">
        <p className="text-sm font-medium">{title}</p>
        <p className="text-xs text-muted-foreground">{detail}</p>
      </div>
      {rows.map((row) => (
        <div key={row.primary} className="flex items-start gap-2 rounded-md px-2 py-1.5">
          <span className={cn("mt-1.5 size-1.5 shrink-0 rounded-full", row.tone ?? "bg-transparent")} aria-hidden />
          <div className="min-w-0">
            <p className="truncate text-sm">{row.primary}</p>
            {row.secondary && <p className="text-xs text-muted-foreground">{row.secondary}</p>}
          </div>
        </div>
      ))}
    </div>
  );
}

/// A working session that delegates, runs in a failing environment and
/// belongs to a bot: every kind of item at once.
function busySession(act: (what: string) => void): MetaItem[] {
  return [
    {
      key: "activity", label: "Working", icon: ActivityIcon, text: "Working", tone: "success", pulse: true,
      tooltip: "Working: a run is in progress", priority: 9, places: PAGE,
    },
    {
      key: "environment", label: "Environment build-box: failed", icon: EnvironmentIcon, text: "build-box", tone: "danger",
      tooltip: "Environment build-box failed to start", priority: 8,
      details: <Details title="Environment" detail="Commands, jobs, and machine files run here."
        rows={[{ primary: "build-box", secondary: "Failed · edit access", tone: "bg-danger" }, { primary: "scratch", secondary: "Paused · wakes on use", tone: "bg-muted-foreground/50" }]} />,
    },
    {
      key: "bot", label: "Run by Release planner", icon: BotIcon, text: "via Release planner",
      tooltip: "Run by a bot: it controls this session's lifecycle and chat input", priority: 7,
      onSelect: () => act("Open the bot Release planner"),
    },
    {
      key: "agents", label: "3 sub-agents", icon: SubAgentIcon, value: 3, text: "3 sub-agents",
      tooltip: "3 sub-agents delegated by this session", priority: 6,
      details: <Details title="Sub-agents" detail="Sessions this one delegated work to."
        rows={[{ primary: "Bisect the flake", secondary: "Working", tone: "bg-success" }, { primary: "Read CI logs", secondary: "Closed" }, { primary: "Draft the fix", secondary: "Idle" }]} />,
    },
    {
      key: "mcp", label: "MCP server GitHub: needs authentication", icon: ToolsIcon, text: "GitHub", tone: "warning",
      tooltip: "GitHub needs authentication", priority: 5, places: PAGE,
      details: <Details title="MCP servers" detail="Remote tools from the universe catalog."
        rows={[{ primary: "GitHub", secondary: "12 tools · Needs authentication", tone: "bg-warning" }]} />,
    },
    {
      key: "workspaces", label: "2 workspaces", icon: WorkspaceIcon, value: 2, text: "2 workspaces", priority: 4, places: PAGE,
      details: <Details title="Files" detail="Workspaces mounted into the agent's virtual file system."
        rows={[{ primary: "lightspeed", secondary: "/repo · read and edit" }, { primary: "notes", secondary: "/notes · read only" }]} />,
    },
    {
      key: "shared", label: "Shared", icon: SharedIcon, text: "Shared",
      tooltip: "Shared: every member of the universe sees this", priority: 3,
    },
    {
      key: "model", label: "claude-opus, 48k tokens of context", icon: ModelIcon, value: "48k", text: "claude-opus · 48k",
      tooltip: "Model and context in use", priority: 2, align: "end",
      onSelect: () => act("Open the model picker"),
    },
    { key: "web", label: "Web search and fetch", icon: WebIcon, text: "Web", priority: 1, places: PAGE },
    { key: "timers", label: "Timers", icon: TimerIcon, text: "Timers", priority: 0, places: PAGE },
  ];
}

const quietSession: MetaItem[] = [
  { key: "environment", label: "No environment", icon: EnvironmentIcon, text: "No environment", placeholder: true, places: ["strip"], priority: 2 },
  { key: "shared", label: "Shared", icon: SharedIcon, text: "Shared", tooltip: "Shared: every member of the universe sees this", priority: 1 },
];

const closedSession: MetaItem[] = [
  { key: "managed", label: "Managed session", icon: ManagedIcon, text: "Managed by Slack", tooltip: "Managed: an integration controls this session's lifecycle and chat input", priority: 2 },
  { key: "closed", label: "Closed", icon: SessionCloseIcon, text: "Closed", tone: "muted", tooltip: <>Closed 11 Nov 2026, 09:12<br />Deleted after 11 Dec 2026, 09:12</>, priority: 3 },
];

/// A bot with events waiting on it, two triggers and a channel.
function botItems(act: (what: string) => void): MetaItem[] {
  return [
    { key: "status", label: "Waiting for approval", icon: ActivityIcon, text: "Waiting for approval", tone: "warning", tooltip: "A run is paused until someone decides on a tool call", priority: 9, places: PAGE },
    {
      key: "pending", label: "3 events waiting", icon: InboxIcon, value: 3, text: "3 waiting", tone: "info",
      tooltip: "3 events waiting to be handled", priority: 8, onSelect: () => act("Open the bot's inbox"),
    },
    {
      key: "sessions", label: "2 open sessions", icon: SessionIcon, value: 2, text: "2 sessions",
      tooltip: "2 open sessions this bot runs", priority: 6,
    },
    { key: "schedule", label: "Schedule: weekdays at 09:00", icon: ScheduleIcon, text: "Weekdays 09:00", priority: 5, places: PAGE },
    { key: "webhook", label: "Webhook trigger", icon: WebhookIcon, text: "Webhook", priority: 4, places: PAGE },
    {
      key: "channel", label: "Telegram channel", icon: ChannelIcon, text: "Telegram", tone: "success", tooltip: "Connected to Telegram", priority: 3,
      details: <Details title="Channels" detail="Where this bot hears and answers." rows={[{ primary: "Telegram", secondary: "@release_planner_bot · connected", tone: "bg-success" }]} />,
    },
    { key: "shared", label: "Shared", icon: SharedIcon, text: "Shared", priority: 2, align: "end" },
  ];
}

const pausedBot: MetaItem[] = [
  { key: "paused", label: "Paused", icon: PauseIcon, text: "Paused", tone: "muted", tooltip: "Paused: triggers do not start runs", priority: 2 },
];

/// A width slider over an example, so its toolbars can be seen giving way.
function Width({ label, min, max, initial, children }: { label: string; min: number; max: number; initial: number; children: (width: number) => ReactNode }) {
  const [width, setWidth] = useState(initial);
  return (
    <div className="grid w-full gap-3">
      <label className="flex items-center gap-3 text-xs text-muted-foreground">
        <span className="shrink-0">{label}</span>
        <input type="range" min={min} max={max} step={4} value={width} onChange={(event) => setWidth(Number(event.target.value))} className="w-56 accent-primary" />
        <span className="font-mono tabular-nums">{width}px</span>
      </label>
      {children(width)}
    </div>
  );
}

/// What the last click on an item did, since the examples go nowhere.
function useActed() {
  const [acted, setActed] = useState<string | null>(null);
  const note = <p className="text-xs text-muted-foreground">{acted ? <>Clicked: <span className="text-foreground">{acted}</span></> : "Click an item: some act, some open details, the rest only explain themselves."}</p>;
  return { act: setActed, note };
}

/// The same items in both modes, with what each item opts into.
export function MetaModesExample() {
  const { act, note } = useActed();
  const items = busySession(act);
  const mark = (on: boolean) => (on ? "✓" : "–");
  return (
    <div className="grid w-full gap-4">
      {PLACES.map((place) => (
        <div key={place} className="grid gap-1.5">
          <span className="font-mono text-[11px] text-muted-foreground">place="{place}" · {metaItemsFor(items, place).length} of {items.length} items</span>
          <MetaToolbar items={items} place={place} wrap label="Session details" />
        </div>
      ))}
      {note}
      <table className="w-full text-left text-xs">
        <thead className="text-muted-foreground">
          <tr>{["Item", "Priority", "Align", "List", "Header", "Strip", "Tone", "Click"].map((head) => <th key={head} className="py-1 pr-3 font-medium">{head}</th>)}</tr>
        </thead>
        <tbody className="font-mono">
          {[...items].sort((a, b) => (b.priority ?? 0) - (a.priority ?? 0)).map((item) => (
            <tr key={item.key} className="border-t">
              <td className="py-1 pr-3">{item.key}</td>
              <td className="py-1 pr-3">{item.priority ?? 0}</td>
              <td className="py-1 pr-3">{item.align ?? "start"}</td>
              {PLACES.map((place) => <td key={place} className="py-1 pr-3">{mark(!item.places || item.places.includes(place))}</td>)}
              <td className="py-1 pr-3">{item.tone ?? "–"}</td>
              <td className="py-1 pr-3">{item.details ? "details" : item.onSelect ? "onSelect" : item.href ? "href" : "–"}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

const SESSION_ROWS = [
  { id: "flaky", title: "Fix the flaky deploy test", line: "Bisecting points at the retry change in #482…", activity: "working" as const },
  { id: "digest", title: "Weekly digest", line: "Sent the digest to #releases.", activity: undefined },
  { id: "cache", title: "Explore caching for the model list", line: "The catalog changes rarely, so a 5 minute cache…", activity: undefined },
];

/// Session rows: their marks beside the title, giving way as the list
/// narrows; the most important stays.
export function MetaSessionListExample() {
  const items: Record<string, MetaItem[]> = { flaky: busySession(() => {}), digest: closedSession, cache: quietSession };
  return (
    <Width label="List width" min={200} max={480} initial={320}>
      {(width) => (
        <div className="flex h-64 overflow-hidden rounded-lg border" style={{ width }}>
          <div className="flex w-full flex-col">
            <ListPaneHeader title="Sessions" count={SESSION_ROWS.length} />
            <ListPaneBody>
              <ul>
                {SESSION_ROWS.map((row) => (
                  <ListRow
                    key={row.id}
                    to={`#meta-${row.id}`}
                    active={row.id === "flaky"}
                    indicator={<ActivityDot activity={row.activity} quiet />}
                    title={row.title}
                    marks={items[row.id]!}
                    meta="2m"
                  >
                    <ListRowLine>{row.line}</ListRowLine>
                  </ListRow>
                ))}
              </ul>
            </ListPaneBody>
          </div>
        </div>
      )}
    </Width>
  );
}

/// A session's page: marks in the header beside the title, tags
/// tags in the strip under it, from the same items.
export function MetaSessionPageExample() {
  const { act, note } = useActed();
  const items = busySession(act);
  return (
    <Width label="Page width" min={320} max={960} initial={720}>
      {(width) => (
        <div className="grid gap-2">
          <div className="overflow-hidden rounded-lg border" style={{ width }}>
            <DetailHeader
              title="Fix the flaky deploy test"
              tags={<MetaToolbar items={items} place="header" expandOnHover label="Session details" className="grow" />}
              actions={[{ key: "share", label: "Share", icon: <SharedIcon />, onSelect: () => act("Share") }]}
            />
            <div className="border-b bg-muted/30 px-4 py-1.5">
              <MetaToolbar items={items} place="strip" label="Session resources" />
            </div>
            <p className="p-4 text-sm text-muted-foreground">The transcript.</p>
          </div>
          {note}
        </div>
      )}
    </Width>
  );
}

/// A bot: its row, its header and its strip, from one list of items.
export function MetaBotExample() {
  const { act, note } = useActed();
  const items = botItems(act);
  return (
    <Width label="Width" min={280} max={960} initial={640}>
      {(width) => (
        <div className="grid gap-3" style={{ width }}>
          <ul className="overflow-hidden rounded-lg border">
            <ListRow
              to="#meta-bot"
              leading={<Avatar className="size-7"><AvatarFallback>R</AvatarFallback></Avatar>}
              indicator={<StatusDot tone="waiting" />}
              title="Release planner"
              marks={items}
              meta="5m"
            >
              <ListRowLine>#14 waiting for approval · shell</ListRowLine>
            </ListRow>
            <ListRow
              to="#meta-bot-paused"
              leading={<Avatar className="size-7"><AvatarFallback>D</AvatarFallback></Avatar>}
              indicator={<StatusDot tone="paused" />}
              title="Deploy checks"
              marks={pausedBot}
              meta="3d"
            >
              <ListRowLine>Paused 3 days ago</ListRowLine>
            </ListRow>
          </ul>
          <div className="overflow-hidden rounded-lg border">
            <DetailHeader
              leading={<Avatar className="size-6"><AvatarFallback>R</AvatarFallback></Avatar>}
              title="Release planner"
              tags={<MetaToolbar items={items} place="header" expandOnHover label="Bot details" className="grow" />}
            />
            <div className="border-b bg-muted/30 px-4 py-1.5">
              <MetaToolbar items={items} place="strip" label="Bot setup" />
            </div>
            <p className="p-4 text-sm text-muted-foreground">The bot's chat.</p>
          </div>
          {note}
        </div>
      )}
    </Width>
  );
}
