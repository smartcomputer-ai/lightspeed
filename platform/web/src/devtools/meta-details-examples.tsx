import { useEffect, useState, type ReactNode } from "react";
import { useLocation } from "react-router-dom";
import {
  ChannelIcon,
  CodeModeIcon,
  ConfigureIcon,
  CopiedIcon,
  CopyIcon,
  EnvironmentIcon,
  GoToIcon,
  NewTabIcon,
  ShowMoreIcon,
  SubAgentIcon,
  TimerIcon,
  ToolsIcon,
  WebIcon,
  WorkspaceIcon,
} from "@/icons";
import type { StatusTone } from "@/components/bot/status";
import {
  MetaDetails,
  MetaDetailsAction,
  MetaDetailsEmpty,
  MetaDetailsFooter,
  MetaDetailsHeading,
  MetaDetailsList,
  MetaDetailsNote,
  MetaDetailsRow,
  RESOURCE_STATUS_TONE,
} from "@/components/meta-details";
import { MetaToolbar, type MetaItem } from "@/components/meta-toolbar";
import { ConversationDetails, type BotConversation } from "@/components/bot/conversations";
import { Popover } from "@/components/ui/popover";
import type { ResourceTone } from "@/lib/sessions/session-resources";

/// Examples of the meta details parts for the UI overview: the content of
/// every meta item popover the app has, drawn open on the popover surface,
/// and a toolbar whose items open them for real.

/// The popover surface, as PopoverContent draws it for a meta item, but in
/// place and always open. A popover root around it lets rows that close
/// their popover work here too.
const SURFACE = "w-80 max-w-full rounded-(--surface-popover-radius) bg-(--surface-popover) surface-filter-popover p-1.5 text-popover-foreground shadow-(--surface-popover-shadow) inset-ring-(length:--surface-popover-border-width) inset-ring-(color:--surface-popover-border)";

function Panel({ label, children }: { label: string; children: ReactNode }) {
  return (
    <figure className="grid content-start gap-1.5">
      <figcaption className="font-mono text-[11px] text-muted-foreground">{label}</figcaption>
      <Popover>
        <div className={SURFACE}>{children}</div>
      </Popover>
    </figure>
  );
}

function Panels({ children }: { children: ReactNode }) {
  return <div className="grid w-full gap-5 sm:grid-cols-2">{children}</div>;
}

/// What the last click did: an action's name, or the row link's target
/// (rows link to #<name> here, so they stay on the page).
function useActed() {
  const [acted, setActed] = useState<string | null>(null);
  const { hash } = useLocation();
  useEffect(() => {
    if (hash) setActed(`Opened ${decodeURIComponent(hash.slice(1))}`);
  }, [hash]);
  const note = (
    <p className="text-xs text-muted-foreground">
      {acted ? <>Clicked: <span className="text-foreground">{acted}</span></> : "Click a row or an action: links and actions close their popover, load more keeps it open."}
    </p>
  );
  return { act: setActed, note };
}

const link = (name: string) => `#${encodeURIComponent(name)}`;

/// The session settings and related page, under each session resource.
function ResourceFooter({ act, page }: { act: (what: string) => void; page?: string }) {
  return (
    <MetaDetailsFooter>
      <MetaDetailsAction icon={ConfigureIcon} onSelect={() => act("Change in session settings")}>Change in session settings</MetaDetailsAction>
      {page && <MetaDetailsAction icon={GoToIcon} href={link(page)}>{page}</MetaDetailsAction>}
    </MetaDetailsFooter>
  );
}

// The session resources, one popover each (session-resources.tsx today).

function EnvironmentDetails({ act }: { act: (what: string) => void }) {
  return (
    <MetaDetails>
      <MetaDetailsHeading title="Environment" description="The agent may switch between the attached environments." />
      <MetaDetailsList label="Environments">
        <MetaDetailsRow primary="build-box" secondary={["Running", "edit access"]} tone={RESOURCE_STATUS_TONE.ok} badges={["Active", "Default"]} />
        <MetaDetailsRow primary="gpu-runner" secondary={["Starting", "read access"]} tone={RESOURCE_STATUS_TONE.busy} />
        <MetaDetailsRow primary="scratch" secondary={["Paused · wakes on use", "edit access"]} tone={RESOURCE_STATUS_TONE.idle} />
        <MetaDetailsRow primary="legacy-ci" secondary="Closed or not in this universe's list" tone={RESOURCE_STATUS_TONE.problem} />
      </MetaDetailsList>
      <MetaDetailsNote>Instructions and skills load from the active environment.</MetaDetailsNote>
      <ResourceFooter act={act} page="Environments" />
    </MetaDetails>
  );
}

function NoEnvironmentDetails({ act }: { act: (what: string) => void }) {
  return (
    <MetaDetails>
      <MetaDetailsHeading title="Environment" description="Commands, jobs, and machine files run here." />
      <MetaDetailsEmpty>No environments are attached.</MetaDetailsEmpty>
      <ResourceFooter act={act} page="Environments" />
    </MetaDetails>
  );
}

function FilesDetails({ act }: { act: (what: string) => void }) {
  return (
    <MetaDetails>
      <MetaDetailsHeading title="Files" description="Workspaces mounted into the agent's virtual file system." />
      <MetaDetailsList label="Workspaces">
        <MetaDetailsRow primary="acme-web" href={link("workspace acme-web")} secondary={[<span key="path" className="font-mono">/repo</span>, "read and edit"]} />
        <MetaDetailsRow primary="design-notes" href={link("workspace design-notes")} secondary={[<span key="path" className="font-mono">/notes</span>, "read only", "pinned snapshot"]} />
        <MetaDetailsRow primary="old-assets" secondary={[<span key="path" className="font-mono">/assets</span>, "read only", "not found"]} tone={RESOURCE_STATUS_TONE.problem} />
      </MetaDetailsList>
      <MetaDetailsNote>Instructions load from these workspaces.</MetaDetailsNote>
      <ResourceFooter act={act} page="Workspaces" />
    </MetaDetails>
  );
}

function McpDetails({ act }: { act: (what: string) => void }) {
  return (
    <MetaDetails>
      <MetaDetailsHeading title="MCP servers" description="Remote tools from the universe catalog." />
      <MetaDetailsList label="MCP servers">
        <MetaDetailsRow primary="GitHub" secondary={["12 of 40 tools", "Connected"]} tone={RESOURCE_STATUS_TONE.ok} badges={["Asks approval"]} />
        <MetaDetailsRow primary="Linear" secondary={["All allowed tools", "Connecting"]} tone={RESOURCE_STATUS_TONE.busy} />
        <MetaDetailsRow primary="Sentry" secondary={["4 tools", "Needs authentication"]} tone={RESOURCE_STATUS_TONE.problem} />
      </MetaDetailsList>
      <ResourceFooter act={act} page="MCP servers" />
    </MetaDetails>
  );
}

function ProfilesDetails({ act }: { act: (what: string) => void }) {
  return (
    <MetaDetails>
      <MetaDetailsHeading title="Sub-agent profiles" description="Profiles the agent may run as sub-agents." />
      <MetaDetailsList label="Profiles">
        {["repo-explorer", "test-runner", "reviewer"].map((profile) => (
          <MetaDetailsRow key={profile} primary={profile} mono href={link(`profile ${profile}`)} />
        ))}
      </MetaDetailsList>
      <MetaDetailsNote>Up to 3 levels deep · 5 open at once.</MetaDetailsNote>
      <ResourceFooter act={act} />
    </MetaDetails>
  );
}

function CodeModeDetails({ act }: { act: (what: string) => void }) {
  return (
    <MetaDetails>
      <MetaDetailsHeading title="Code mode" description="The agent may compose its available tools with JavaScript." />
      <MetaDetailsNote>Ordinary tool calls remain available alongside code execution.</MetaDetailsNote>
      <ResourceFooter act={act} />
    </MetaDetails>
  );
}

function WebDetails({ act }: { act: (what: string) => void }) {
  return (
    <MetaDetails>
      <MetaDetailsHeading title="Web" description="Independent of MCP servers and the environment's network." />
      <MetaDetailsList label="Web tools">
        <MetaDetailsRow primary="Search" secondary="Enabled" tone={RESOURCE_STATUS_TONE.ok} />
        <MetaDetailsRow primary="Fetch pages" secondary="Off" tone={RESOURCE_STATUS_TONE.idle} />
      </MetaDetailsList>
      <ResourceFooter act={act} />
    </MetaDetails>
  );
}

function TimersDetails({ act }: { act: (what: string) => void }) {
  return (
    <MetaDetails>
      <MetaDetailsHeading title="Timers" description="The agent may wait, sleep, and await delayed work." />
      <ResourceFooter act={act} />
    </MetaDetails>
  );
}

// The sub-agents a session delegated to (session-meta.tsx today).

const SUB_AGENTS: Array<{ name: string; profile: string; tone: StatusTone; state: string; when: string; closed?: boolean }> = [
  { name: "Bisect the flaky scheduler test", profile: "test-runner", tone: "live", state: "Working", when: "just now" },
  { name: "Approve the migration dry run", profile: "db-operator", tone: "waiting", state: "Waiting for approval", when: "2m ago" },
  { name: "Wait for the nightly build", profile: "ci-watch", tone: "awaiting", state: "Awaiting", when: "8m ago" },
  { name: "Draft the fix", profile: "implementer", tone: "idle", state: "Idle", when: "12m ago" },
  { name: "Run the integration suite", profile: "test-runner", tone: "attention", state: "Last run failed", when: "20m ago" },
  { name: "Audit src/scheduler for wall-clock dependencies", profile: "repo-explorer", tone: "closed", state: "Closed", when: "1d ago", closed: true },
];

const MORE_SUB_AGENTS = Array.from({ length: 6 }, (_, index) => ({
  name: `Check shard ${index + 1} of the test matrix`, profile: "test-runner", tone: "closed" as StatusTone, state: "Closed", when: "1d ago", closed: true,
}));

function SubAgentsDetails({ act, many = false }: { act: (what: string) => void; many?: boolean }) {
  const [loaded, setLoaded] = useState(false);
  const [loading, setLoading] = useState(false);
  const rows = many && loaded ? [...SUB_AGENTS, ...MORE_SUB_AGENTS] : SUB_AGENTS;
  const loadMore = () => {
    setLoading(true);
    act("Load more sub-agents");
    window.setTimeout(() => {
      setLoaded(true);
      setLoading(false);
    }, 800);
  };
  return (
    <MetaDetails>
      <MetaDetailsHeading title="Sub-agents" description="Sessions this one delegated work to." />
      <MetaDetailsList label="Sub-agents">
        {rows.map((child) => (
          <MetaDetailsRow
            key={child.name}
            primary={child.name}
            secondary={[child.profile, child.state, child.when]}
            tone={child.tone}
            muted={child.closed}
            href={link(`sub-agent ${child.name}`)}
          />
        ))}
        <MetaDetailsRow primary={<span className="font-mono text-xs">session-sub-9f2c41a7…</span>} secondary={["reviewer", "Idle", "1h ago"]} tone="idle" href={link("an unnamed sub-agent")} />
      </MetaDetailsList>
      {many && !loaded && (
        <MetaDetailsFooter>
          <MetaDetailsAction icon={ShowMoreIcon} onSelect={loadMore} keepOpen busy={loading}>{loading ? "Loading…" : "Load more sub-agents"}</MetaDetailsAction>
        </MetaDetailsFooter>
      )}
    </MetaDetails>
  );
}

// A workspace file's content (workspace-file-meta.tsx today).

const HASH = "sha256:9f2c41a7e0b35d6c8814f2a9b7c3e15d06a4f8b2c9e71d3a5f60b8c2d4e917aa";

function FileContentDetails({ unsaved = false }: { unsaved?: boolean }) {
  const [copied, setCopied] = useState(false);
  const Copy = copied ? CopiedIcon : CopyIcon;
  return (
    <MetaDetails>
      <MetaDetailsHeading
        title="Content"
        description="Stored by its content: the same bytes always have this hash. Loaded at workspace revision 42."
      />
      <MetaDetailsList>
        <MetaDetailsRow
          primary={HASH}
          mono
          inset={false}
          end={(
            <button
              type="button"
              aria-label={copied ? "Copied" : "Copy content hash"}
              onClick={() => {
                setCopied(true);
                window.setTimeout(() => setCopied(false), 1500);
              }}
              className="cursor-action rounded p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground"
            >
              <Copy className="size-3.5" />
            </button>
          )}
        />
      </MetaDetailsList>
      <MetaDetailsFooter>
        <MetaDetailsAction icon={NewTabIcon} href={link("the blob viewer")}>
          {unsaved ? "Open saved version in blob viewer" : "Open in blob viewer"}
        </MetaDetailsAction>
      </MetaDetailsFooter>
    </MetaDetails>
  );
}

// A bot's channels (the meta toolbar examples).

function ChannelsDetails({ act }: { act: (what: string) => void }) {
  return (
    <MetaDetails>
      <MetaDetailsHeading title="Channels" description="Where this bot hears and answers." />
      <MetaDetailsList label="Channels">
        <MetaDetailsRow primary="Telegram" secondary={["@release_planner_bot", "connected"]} tone="ready" />
        <MetaDetailsRow primary="WhatsApp" secondary={["+41 44 000 00 00", "reconnecting"]} tone="waiting" />
      </MetaDetailsList>
      <MetaDetailsFooter>
        <MetaDetailsAction icon={ConfigureIcon} onSelect={() => act("Change in bot settings")}>Change in bot settings</MetaDetailsAction>
        <MetaDetailsAction icon={GoToIcon} href={link("Channels")}>Channels</MetaDetailsAction>
      </MetaDetailsFooter>
    </MetaDetails>
  );
}

// A bot's conversations, as its page's conversation switcher lists them:
// Main and its threads (components/bot/conversations).

const CONVERSATIONS: BotConversation[] = [
  { id: "main", kind: "main", title: "Main", line: "#8 handled · Planned LIN-1422 into three tasks", tone: "idle", closed: false, atMs: Date.now() - 40 * 60_000 },
  { id: "lin-1421-c", kind: "thread", title: "lin-1421-c", line: "Working on #9", tone: "live", closed: false, atMs: Date.now() - 60_000 },
  { id: "lin-1421-b", kind: "thread", title: "lin-1421-b", line: "#7 run failed · Provider returned 529", tone: "attention", failed: true, closed: false, atMs: Date.now() - 2 * 3_600_000 },
  { id: "digest", kind: "thread", title: "Weekly digest", line: "#5 handled · Sent the digest to #eng", tone: "idle", closed: false, atMs: Date.now() - 26 * 3_600_000 },
];

function ConversationsDetails() {
  return <ConversationDetails conversations={CONVERSATIONS} currentId="lin-1421-c" href={(id) => link(`conversation ${id}`)} bot="Implementer" />;
}

/// Every usage, drawn open.
export function MetaDetailsUsagesExample() {
  const { act, note } = useActed();
  return (
    <div className="grid w-full gap-4">
      {note}
      <Panels>
        <Panel label="Sub-agents · linked rows, every state"><SubAgentsDetails act={act} /></Panel>
        <Panel label="Environment · dots, badges, note, footer"><EnvironmentDetails act={act} /></Panel>
        <Panel label="Environment · none attached"><NoEnvironmentDetails act={act} /></Panel>
        <Panel label="Files · linked rows with paths, one missing"><FilesDetails act={act} /></Panel>
        <Panel label="MCP servers · connected, connecting, failing"><McpDetails act={act} /></Panel>
        <Panel label="Sub-agent profiles · linked code names"><ProfilesDetails act={act} /></Panel>
        <Panel label="Web · on and off"><WebDetails act={act} /></Panel>
        <Panel label="Code mode · heading and note only"><CodeModeDetails act={act} /></Panel>
        <Panel label="Timers · heading and footer only"><TimersDetails act={act} /></Panel>
        <Panel label="Workspace file content · code row, copy, footer action"><FileContentDetails /></Panel>
        <Panel label="Bot channels · steady and reconnecting"><ChannelsDetails act={act} /></Panel>
        <Panel label="Bot conversations · the open one marked"><ConversationsDetails /></Panel>
      </Panels>
    </div>
  );
}

/// A long list: it scrolls inside the popover, and load more keeps the
/// popover open while it loads.
export function MetaDetailsLongListExample() {
  const { act, note } = useActed();
  return (
    <div className="grid w-full gap-4">
      {note}
      <Panels>
        <Panel label="Sub-agents · more than a page"><SubAgentsDetails act={act} many /></Panel>
      </Panels>
    </div>
  );
}

/// The parts on their own.
export function MetaDetailsPartsExample() {
  const { act, note } = useActed();
  const tones: StatusTone[] = ["ready", "live", "awaiting", "waiting", "paused", "attention", "idle", "closed"];
  const resourceTones: ResourceTone[] = ["ok", "busy", "idle", "problem"];
  const [busy, setBusy] = useState(false);
  return (
    <div className="grid w-full gap-4">
      {note}
      <Panels>
        <Panel label="MetaDetailsRow · tone, each status dot">
          <MetaDetails>
            <MetaDetailsList>
              {tones.map((tone) => <MetaDetailsRow key={tone} primary={tone} tone={tone} muted={tone === "closed"} />)}
            </MetaDetailsList>
          </MetaDetails>
        </Panel>
        <Panel label="RESOURCE_STATUS_TONE · resource tones as dots">
          <MetaDetails>
            <MetaDetailsList>
              {resourceTones.map((tone) => (
                <MetaDetailsRow key={tone} primary={tone} secondary={`→ ${RESOURCE_STATUS_TONE[tone]}`} tone={RESOURCE_STATUS_TONE[tone]} />
              ))}
            </MetaDetailsList>
          </MetaDetails>
        </Panel>
        <Panel label="MetaDetailsRow · plain, link, action, icon, inset">
          <MetaDetails>
            <MetaDetailsList>
              <MetaDetailsRow primary="Plain" secondary="Not focusable, no highlight" />
              <MetaDetailsRow primary="Link" secondary="href: highlights, closes" tone="ready" href={link("a link row")} />
              <MetaDetailsRow primary="Action" secondary="onSelect: highlights, closes" tone="idle" onSelect={() => act("An action row")} />
              <MetaDetailsRow primary="External link" secondary="href and external: a new tab" icon={NewTabIcon} href="https://example.com" external />
              <MetaDetailsRow primary="Inset off" secondary="inset={false}: no dot's room" inset={false} />
            </MetaDetailsList>
          </MetaDetails>
        </Panel>
        <Panel label="MetaDetailsRow · badges, facts, muted, mono, end">
          <MetaDetails>
            <MetaDetailsList>
              <MetaDetailsRow primary="build-box" badges={["Active", "Default", false]} secondary={["Running", "edit access", null, ""]} tone="ready" />
              <MetaDetailsRow primary="A very long name that does not fit on one line of the popover" secondary="Names cut; facts wrap onto further lines when they run long like this one does" tone="idle" />
              <MetaDetailsRow primary="Closed thing" secondary="muted" tone="closed" muted />
              <MetaDetailsRow primary="src/components/meta-details.tsx" mono secondary="mono: breaks anywhere" />
              <MetaDetailsRow primary="With trailing controls" secondary="end" end={<button type="button" className="cursor-action rounded p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground" aria-label="Copy" onClick={() => act("Copy")}><CopyIcon className="size-3.5" /></button>} />
            </MetaDetailsList>
          </MetaDetails>
        </Panel>
        <Panel label="MetaDetailsHeading, MetaDetailsEmpty, MetaDetailsNote">
          <MetaDetails>
            <MetaDetailsHeading title="Heading" description="A muted line saying what the item is." />
            <MetaDetailsEmpty>Nothing to list.</MetaDetailsEmpty>
            <MetaDetailsNote>A note under the rows, after a separator.</MetaDetailsNote>
          </MetaDetails>
        </Panel>
        <Panel label="MetaDetailsFooter · actions, keep open, busy, external">
          <MetaDetails>
            <MetaDetailsHeading title="Footer actions" description="One per line, under a separator." />
            <MetaDetailsFooter>
              <MetaDetailsAction icon={ConfigureIcon} onSelect={() => act("Main action")}>Main action</MetaDetailsAction>
              <MetaDetailsAction icon={GoToIcon} href={link("the related page")}>Related page</MetaDetailsAction>
              <MetaDetailsAction
                icon={ShowMoreIcon}
                keepOpen
                busy={busy}
                onSelect={() => {
                  setBusy(true);
                  act("Keep open");
                  window.setTimeout(() => setBusy(false), 1200);
                }}
              >
                {busy ? "Loading…" : "Keep open (load more)"}
              </MetaDetailsAction>
              <MetaDetailsAction icon={NewTabIcon} href="https://example.com" external>External, in a new tab</MetaDetailsAction>
            </MetaDetailsFooter>
          </MetaDetails>
        </Panel>
      </Panels>
    </div>
  );
}

/// The same content behind real meta items: click one to open it.
export function MetaDetailsInToolbarExample() {
  const { act, note } = useActed();
  const items: MetaItem[] = [
    { key: "subagents", label: "7 sub-agents", icon: SubAgentIcon, value: 7, text: "7 sub-agents", priority: 9, details: <SubAgentsDetails act={act} /> },
    { key: "environment", label: "Environment: build-box", icon: EnvironmentIcon, text: "build-box", tone: "success", priority: 8, details: <EnvironmentDetails act={act} /> },
    { key: "files", label: "3 workspaces", icon: WorkspaceIcon, value: 3, text: "3 workspaces", tone: "danger", priority: 7, details: <FilesDetails act={act} /> },
    { key: "mcp", label: "3 MCP servers", icon: ToolsIcon, value: 3, text: "3 MCP servers", tone: "danger", priority: 6, details: <McpDetails act={act} /> },
    { key: "codeMode", label: "Code mode", icon: CodeModeIcon, text: "Code mode", priority: 4, details: <CodeModeDetails act={act} /> },
    { key: "web", label: "Web search", icon: WebIcon, text: "Web search", priority: 3, details: <WebDetails act={act} /> },
    { key: "timers", label: "Timers", icon: TimerIcon, text: "Timers", priority: 2, details: <TimersDetails act={act} /> },
    { key: "content", label: "Content hash", value: <span className="font-mono">9f2c41a7</span>, text: <span className="font-mono">9f2c41a7</span>, priority: 1, details: <FileContentDetails /> },
    { key: "channels", label: "2 channels", icon: ChannelIcon, value: 2, text: "2 channels", tone: "warning", priority: 0, details: <ChannelsDetails act={act} /> },
  ];
  return (
    <div className="grid w-full gap-3">
      <div className="rounded-lg border px-3 py-2">
        <MetaToolbar items={items} place="strip" wrap label="Example details" />
      </div>
      {note}
    </div>
  );
}
