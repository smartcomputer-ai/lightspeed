import { Fragment, useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { idColor } from "@/lib/identity-colors";
import { BotIcon } from "@/icons";
import { StatusDot, type StatusTone } from "@/components/status-dot";
import { ListGroupLabel, ListPaneBody, ListPaneHeader, ListRow, ListRowLine, type ListExpand, type ListRowTree } from "@/components/list-pane";

/// A switch between a list's expand modes (ListExpand), over a list example,
/// with a line saying how its rows open in the chosen one.
export function ExpandSwitch({ value, onChange, hint }: {
  value: ListExpand;
  onChange: (value: ListExpand) => void;
  hint: ReactNode;
}) {
  const modes: Array<[ListExpand, string]> = [["none", "Off"], ["dedicated", "Dedicated"], ["title", "Title"]];
  return (
    <div className="flex flex-wrap items-center gap-2 border-b px-4 py-2 text-xs">
      <span className="text-muted-foreground">Expand</span>
      <div className="flex gap-1 rounded-lg bg-muted p-1" role="radiogroup" aria-label="Expand mode">
        {modes.map(([mode, label]) => (
          <Button
            key={mode}
            size="xs"
            variant={value === mode ? "outline" : "ghost"}
            role="radio"
            aria-checked={value === mode}
            onClick={() => onChange(mode)}
          >
            {label}
          </Button>
        ))}
      </div>
      <span className="text-muted-foreground">{hint}</span>
    </div>
  );
}

/// Examples of expandable list rows for the UI overview: rows whose
/// children open and close from a caret before their content, while the
/// row itself still opens its detail.

interface ExampleNode {
  id: string;
  title: string;
  line: string;
  tone: StatusTone;
  meta: string;
  closed?: boolean;
  /** A bot's avatar, for a root row of the bots list. */
  bot?: string;
  children?: ExampleNode[];
}

/// The bots list: each bot (standing for its Main conversation) with its
/// threads, and the sub-agents under the conversation that delegated to them.
const BOTS: ExampleNode[] = [
  {
    id: "implementer", title: "Implementer", bot: "implementer", tone: "live", meta: "1h", line: "Working on #8 · task.ready",
    children: [
      {
        id: "lin-1421-c", title: "lin-1421-c", tone: "live", meta: "1h", line: "Thread · Working",
        children: [{ id: "metrics-tests", title: "Tests for src/ratelimit/metrics.ts", tone: "live", meta: "2m", line: "test-writer · Working" }],
      },
      {
        id: "lin-1421-b", title: "lin-1421-b", tone: "idle", meta: "2h", line: "Thread",
        children: [{ id: "config-read", title: "How is configuration read?", tone: "closed", closed: true, meta: "3h", line: "repo-explorer · Closed" }],
      },
      {
        id: "lin-1421-a", title: "lin-1421-a", tone: "idle", meta: "3h", line: "Thread",
        children: [
          { id: "rate-limit", title: "What rate-limiting code exists today?", tone: "closed", closed: true, meta: "3h", line: "repo-explorer · Closed" },
          {
            id: "bucket-tests", title: "Tests for src/ratelimit/bucket.ts", tone: "closed", closed: true, meta: "3h", line: "test-writer · Closed",
            children: [{ id: "fake-time", title: "How do existing tests fake time?", tone: "closed", closed: true, meta: "3h", line: "repo-explorer · Closed" }],
          },
        ],
      },
      { id: "digest", title: "Weekly digest", tone: "idle", meta: "1d", line: "One-off" },
    ],
  },
  {
    id: "planner", title: "Planner", bot: "planner", tone: "idle", meta: "1h", line: "#6 handled · Task c done",
    children: [
      { id: "lin-1421", title: "lin-1421", tone: "idle", meta: "1h", line: "Thread" },
      { id: "lin-1388", title: "lin-1388", tone: "idle", meta: "2d", line: "Thread" },
    ],
  },
  { id: "ci-watch", title: "CI Watch", bot: "ci-watch", tone: "idle", meta: "1h", line: "#7 handled · Runs 9193 and 9194 green" },
  {
    id: "pr-reviewer", title: "PR Reviewer", bot: "pr-reviewer", tone: "attention", meta: "1h", line: "#11 run failed · Provider error",
    children: [{ id: "pr-493", title: "PR #493", tone: "waiting", meta: "1h", line: "Thread · Waiting for approval" }],
  },
];

/// Every row with children, for "open all".
function parents(nodes: ExampleNode[]): string[] {
  return nodes.flatMap((node) => (node.children?.length ? [node.id, ...parents(node.children)] : []));
}

/// The rows of one level, and below each open one the next level. A row with
/// children gets a caret; when any of the level has one, the rest keep its
/// room so their titles line up, and a level where none has one takes none.
function ExpandableRows({ nodes, depth, lines, open, onToggle, active, onOpen }: {
  nodes: ExampleNode[];
  depth: number;
  lines: boolean[];
  open: Set<string>;
  onToggle: (id: string) => void;
  active: string;
  onOpen: (id: string) => void;
}) {
  const caretRoom = nodes.some((node) => (node.children?.length ?? 0) > 0);
  return nodes.map((node, index) => {
    const last = index === nodes.length - 1;
    const kids = node.children ?? [];
    const isOpen = kids.length > 0 && open.has(node.id);
    const tree: ListRowTree = depth === 0 ? { depth, children: isOpen } : { depth, last, lines, children: isOpen };
    return (
      <Fragment key={node.id}>
        <ListRow
          to={`#${node.id}`}
          active={node.id === active}
          entryProps={{ onClick: () => onOpen(node.id) }}
          tree={tree}
          {...(kids.length > 0
            ? { expander: { open: isOpen, onToggle: () => onToggle(node.id), label: `${isOpen ? "Hide" : "Show"} ${kids.length} under ${node.title}` } }
            : {})}
          caretRoom={caretRoom}
          leading={node.bot ? <BotIcon style={{ color: idColor(node.bot) }} size={30} /> : undefined}
          indicator={<StatusDot tone={node.tone} />}
          title={node.title}
          muted={node.closed}
          meta={node.meta}
        >
          <ListRowLine tone={node.tone === "attention" ? "destructive" : undefined}>{node.line}</ListRowLine>
        </ListRow>
        {isOpen && (
          <ExpandableRows
            nodes={kids}
            depth={depth + 1}
            lines={depth === 0 ? [] : [...lines, !last]}
            open={open}
            onToggle={onToggle}
            active={active}
            onOpen={onOpen}
          />
        )}
      </Fragment>
    );
  });
}

function useOpenRows(initial: string[]) {
  const [open, setOpen] = useState<Set<string>>(() => new Set(initial));
  const toggle = (id: string) =>
    setOpen((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  return { open, setOpen, toggle };
}

/// The bots list with expandable rows: Implementer open to its threads and
/// one thread's sub-agents, the rest closed. Opening a row (clicking it)
/// opens its children too, as selecting a bot does.
export function ExpandableBotListExample() {
  const { open, setOpen, toggle } = useOpenRows(["implementer", "lin-1421-c"]);
  const [active, setActive] = useState("implementer");
  const all = parents(BOTS);
  return (
    <div className="grid gap-3">
      <div className="flex h-[34rem] w-96 max-w-full flex-col overflow-hidden rounded-xl border">
        <ListPaneHeader title="Bots" count={BOTS.length} />
        <ListPaneBody expand="dedicated">
          <ListGroupLabel>Active</ListGroupLabel>
          <ul>
            <ExpandableRows
              nodes={BOTS}
              depth={0}
              lines={[]}
              open={open}
              onToggle={toggle}
              active={active}
              onOpen={(id) => {
                setActive(id);
                setOpen((current) => new Set([...current, id]));
              }}
            />
          </ul>
        </ListPaneBody>
      </div>
      <div className="flex flex-wrap items-center gap-3 text-xs text-muted-foreground">
        <button type="button" className="cursor-action hover:text-foreground hover:underline" onClick={() => setOpen(new Set(all))}>Open all</button>
        <button type="button" className="cursor-action hover:text-foreground hover:underline" onClick={() => setOpen(new Set())}>Close all</button>
        <span>Click a caret to open or close its rows; click a row to open it. On a focused row, → opens and ← closes.</span>
      </div>
    </div>
  );
}

/// The same rows without avatars, as a sessions list would draw them.
const SESSIONS: ExampleNode[] = [
  {
    id: "flaky", title: "Fix flaky scheduler test", tone: "idle", meta: "1d", line: "Done — the scheduler suite is green",
    children: [
      {
        id: "audit", title: "Audit src/scheduler for wall-clock dependencies", tone: "closed", closed: true, meta: "1d", line: "Audited src/scheduler (4 files)",
        children: [{ id: "clock", title: "Find every Date.now call", tone: "closed", closed: true, meta: "1d", line: "12 calls in 4 files" }],
      },
      { id: "bisect", title: "Bisect the flake", tone: "live", meta: "now", line: "Running the suite 50 times" },
    ],
  },
  { id: "auth", title: "Explain the auth middleware", tone: "live", meta: "now", line: "Write that up as a short ADR" },
  { id: "ci", title: "Why is CI slow this week?", tone: "idle", meta: "3d", line: "Everything is flat except install" },
];

export function ExpandableSessionListExample() {
  const { open, toggle } = useOpenRows(["flaky"]);
  const [active, setActive] = useState("flaky");
  return (
    <div className="flex h-[22rem] w-96 max-w-full flex-col overflow-hidden rounded-xl border">
      <ListPaneHeader title="Sessions" count={SESSIONS.length} />
      <ListPaneBody expand="dedicated">
        <ul>
          <ExpandableRows nodes={SESSIONS} depth={0} lines={[]} open={open} onToggle={toggle} active={active} onOpen={setActive} />
        </ul>
      </ListPaneBody>
    </div>
  );
}
