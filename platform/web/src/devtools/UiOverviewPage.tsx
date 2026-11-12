import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useDocumentTitle } from "@/lib/document-title";
import { Link, NavLink, useParams } from "react-router-dom";
import { DEFAULT_UNIVERSE_ICON, UNIVERSE_ICONS } from "@lightspeed-ai/platform-shared";
import { AddIcon, BotIcon, CopyIcon, CopyLinkIcon, DarkThemeIcon, DeleteIcon, DownloadIcon, EditIcon, EmailIcon, InboxIcon, MoreIcon, NewFileIcon, NewFolderIcon, LightThemeIcon, NotificationIcon, RefreshIcon, SearchIcon, SelectModeIcon, SettingsIcon, SwitcherIcon, SystemThemeIcon } from "@/icons";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogMedia,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { Avatar, AvatarBadge, AvatarFallback, AvatarGroup, AvatarGroupCount } from "@/components/ui/avatar";
import { Bubble, BubbleContent, BubbleGroup, BubbleReactions } from "@/components/ui/bubble";
import { Card, CardAction, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { Combobox, ComboboxContent, ComboboxEmpty, ComboboxInput, ComboboxItem, ComboboxList } from "@/components/ui/combobox";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuShortcut,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Field,
  FieldContent,
  FieldDescription,
  FieldError,
  FieldGroup,
  FieldLabel,
  FieldLegend,
  FieldSeparator,
  FieldSet,
} from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { InputGroup, InputGroupAddon, InputGroupButton, InputGroupInput, InputGroupText, InputGroupTextarea } from "@/components/ui/input-group";
import { Label } from "@/components/ui/label";
import { Marker, MarkerContent, MarkerIcon } from "@/components/ui/marker";
import { Message, MessageContent, MessageFooter, MessageHeader } from "@/components/ui/message";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { ProgressSteps } from "@/components/ui/progress-steps";
import { Select, SelectContent, SelectGroup, SelectItem, SelectLabel, SelectSeparator, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Separator } from "@/components/ui/separator";
import { SettingsDisclosure, SettingsGroup } from "@/components/ui/settings-disclosure";
import { Sheet, SheetContent, SheetDescription, SheetFooter, SheetHeader, SheetTitle, SheetTrigger } from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import {
  IdText,
  Table,
  TableActionsCell,
  TableBody,
  TableCaption,
  TableCard,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  TableTitleCell,
} from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Textarea } from "@/components/ui/textarea";
import { InfoBox } from "@/components/ui/info-box";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { EmptyState, PageHeader, SectionHeader } from "@/components/page";
import { StatusDot, type BotTone } from "@/components/bot/status";
import { useTheme, type Theme } from "@/lib/theme";
import { cn } from "@/lib/utils";
import * as Icons from "@/icons";
import type { IconComponent } from "@/icons";
import ROLES_SOURCE from "@/icons/roles.ts?raw";
import OWN_SOURCE from "./UiOverviewPage.tsx?raw";
import { GROUP_ORDER, StepRow, groupStyle } from "@/components/session/tool-trace";
import { BotAvatar, botHue } from "@/components/bot/face";
import { UNIVERSE_NAV } from "@/components/universe-nav";
import { ListGroupLabel, ListPaneBody, ListPaneHeader, ListPaneToolbar, ListRow, ListRowLine, PaneBar, type ListRowTree } from "@/components/list-pane";
import { LightspeedLogo, LightspeedMark } from "@/components/brand/lightspeed-logo";
import { Starscape } from "@/components/starscape/starscape";
import { PAGE_NEBULA_SEED, PageNebula } from "@/components/starscape/page-nebula";
import { UniverseIcon } from "@/components/universe-icon";
import { HEADER_STARSCAPE_CLASS, HEADER_STARSCAPE_CONFIG, HEADER_STARSCAPE_FADE } from "@/components/universe-switcher";
import { collectRoleUsage, iconRoles, libraryGlyphs, type IconRole, type RoleGlyph } from "./icon-usage";
import { MetaBotExample, MetaModesExample, MetaSessionListExample, MetaSessionPageExample } from "./meta-toolbar-examples";

/// Every shared UI component and its variations, one page per category,
/// for reviewing the design system in both themes: the overview's tiles
/// lead to them, the sidebar between them. Development builds only: the
/// routes are not compiled into production.

const BASE = "/debug/ui-overview";

const CATEGORIES = [
  { id: "branding", title: "Branding", summary: "The logo, the mark and the starscape." },
  { id: "typography", title: "Typography", summary: "Headers, sizes, weights and text styles." },
  { id: "colors", title: "Colors", summary: "Surfaces, status colors, radius and shadows." },
  { id: "icons", title: "Icons", summary: "Every icon role and where it is used." },
  { id: "entities", title: "Entities", summary: "Run steps and tool families, by icon and color." },
  { id: "buttons", title: "Buttons", summary: "Variants, sizes, icons and states." },
  { id: "badges", title: "Badges & status", summary: "Badges, info boxes and status dots." },
  { id: "meta", title: "Meta toolbar", summary: "An entity's facts in rows, headers and strips." },
  { id: "inputs", title: "Inputs", summary: "Text inputs, text areas, input groups and labels." },
  { id: "selects", title: "Selects & comboboxes", summary: "Selects and comboboxes." },
  { id: "toggles", title: "Checkboxes & switches", summary: "Checkboxes and switches." },
  { id: "forms", title: "Forms", summary: "Field layouts, field sets and settings disclosures." },
  { id: "menus", title: "Menus", summary: "Dropdown menus." },
  { id: "overlays", title: "Dialogs & overlays", summary: "Dialogs, sheets, popovers and tooltips." },
  { id: "navigation", title: "Navigation", summary: "Tabs and progress steps." },
  { id: "lists", title: "Lists", summary: "List panes with nested rows, status dots and selection; the file list." },
  { id: "cards", title: "Cards & layout", summary: "Cards, empty states, separators and collapsibles." },
  { id: "tables", title: "Tables", summary: "Table cards and id text." },
  { id: "data", title: "Data display", summary: "Avatars, markers and skeletons." },
  { id: "messages", title: "Messages", summary: "Conversations and chat bubbles." },
] as const;

type CategoryId = (typeof CATEGORIES)[number]["id"];

/// Each category's page.
const SECTIONS: Record<CategoryId, () => ReactNode> = {
  branding: BrandingSection,
  typography: TypographySection,
  colors: ColorsSection,
  icons: IconsSection,
  entities: EntitiesSection,
  buttons: ButtonsSection,
  badges: BadgesSection,
  meta: MetaToolbarSection,
  inputs: InputsSection,
  selects: SelectsSection,
  toggles: TogglesSection,
  forms: FormsSection,
  menus: MenusSection,
  overlays: OverlaysSection,
  navigation: NavigationSection,
  lists: ListsSection,
  cards: CardsSection,
  tables: TablesSection,
  data: DataSection,
  messages: MessagesSection,
};

const isCategory = (id: string | undefined): id is CategoryId => CATEGORIES.some((category) => category.id === id);

export function UiOverviewPage() {
  const { section } = useParams();
  // A new page starts at its top.
  useEffect(() => {
    window.scrollTo(0, 0);
  }, [section]);
  const Section = isCategory(section) ? SECTIONS[section] : undefined;
  useDocumentTitle([CATEGORIES.find((category) => category.id === section)?.title, "UI overview"]);
  return (
    <div className="flex min-h-svh bg-background text-foreground">
      <nav className="sticky top-0 hidden h-svh w-56 shrink-0 overflow-y-auto border-r bg-sidebar p-4 md:block" aria-label="Categories">
        <Link to={BASE} className="mb-1 block text-sm font-semibold hover:underline">UI overview</Link>
        <p className="mb-4 text-xs text-muted-foreground">Development only</p>
        <ul className="grid gap-0.5">
          {CATEGORIES.map((category) => (
            <li key={category.id}>
              <NavLink
                to={`${BASE}/${category.id}`}
                className={({ isActive }) => cn(
                  "block rounded-md px-2 py-1 text-sm text-muted-foreground hover:bg-sidebar-accent hover:text-sidebar-accent-foreground",
                  isActive && "bg-sidebar-accent font-medium text-sidebar-accent-foreground",
                )}
              >
                {category.title}
              </NavLink>
            </li>
          ))}
        </ul>
      </nav>
      <main className="min-w-0 flex-1 px-6 py-8 md:px-10">
        {/* Zero height so it takes no room: the switch floats top-right and stays there while the page scrolls. */}
        <div className="sticky top-3 z-20 mx-auto flex h-0 max-w-5xl justify-end">
          <ThemeSwitch />
        </div>
        <div className="mx-auto grid max-w-5xl gap-14">
          {Section ? (
            <>
              <Link to={BASE} className="w-fit pt-12 text-sm text-muted-foreground hover:text-foreground sm:pt-0">← UI overview</Link>
              <Section />
            </>
          ) : (
            <>
              <div className="grid gap-1 pt-12 sm:pt-0 sm:pr-64">
                <h1 className="text-2xl font-semibold tracking-tight">UI overview</h1>
                <p className="text-sm text-muted-foreground">
                  The shared components in <code className="font-mono text-xs">src/components/ui</code> and their variations, one page per category.
                </p>
              </div>
              <CategoryTiles />
            </>
          )}
        </div>
      </main>
    </div>
  );
}

/// The overview: a tile per category, leading to its page.
function CategoryTiles() {
  return (
    <ul className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3" aria-label="Categories">
      {CATEGORIES.map((category) => (
        <li key={category.id} className="grid">
          <Link to={`${BASE}/${category.id}`} className="block outline-none">
            <Card size="sm" className="h-full transition-colors hover:bg-muted/50">
              <CardHeader>
                <CardTitle>{category.title}</CardTitle>
                <CardDescription>{category.summary}</CardDescription>
              </CardHeader>
            </Card>
          </Link>
        </li>
      ))}
    </ul>
  );
}

function ThemeSwitch() {
  const { theme, setTheme } = useTheme();
  const options: Array<{ value: Theme; label: string; icon: ReactNode }> = [
    { value: "light", label: "Light", icon: <LightThemeIcon /> },
    { value: "dark", label: "Dark", icon: <DarkThemeIcon /> },
    { value: "system", label: "System", icon: <SystemThemeIcon /> },
  ];
  return (
    <div className="flex h-fit gap-1 rounded-lg bg-muted p-1 shadow-md ring-1 ring-foreground/10" role="radiogroup" aria-label="Theme">
      {options.map((option) => (
        <Button
          key={option.value}
          size="sm"
          variant={theme === option.value ? "outline" : "ghost"}
          role="radio"
          aria-checked={theme === option.value}
          onClick={() => setTheme(option.value)}
        >
          {option.icon}
          {option.label}
        </Button>
      ))}
    </div>
  );
}

function Category({ id, description, children }: { id: CategoryId; description?: string; children: ReactNode }) {
  const title = CATEGORIES.find((category) => category.id === id)?.title ?? id;
  return (
    <section id={id} className="grid gap-5" aria-labelledby={`${id}-title`}>
      <div className="grid gap-1 border-b pb-3">
        <h1 id={`${id}-title`} className="text-2xl font-semibold tracking-tight">{title}</h1>
        {description && <p className="text-sm text-muted-foreground">{description}</p>}
      </div>
      {children}
    </section>
  );
}

/// One named example: a label over the rendered variations.
function Specimen({ title, note, className, children }: { title: string; note?: string; className?: string; children: ReactNode }) {
  return (
    <div className="grid gap-2">
      <div className="flex flex-wrap items-baseline gap-x-2">
        <h3 className="text-sm font-medium">{title}</h3>
        {note && <span className="text-xs text-muted-foreground">{note}</span>}
      </div>
      <div className={cn("flex flex-wrap items-center gap-3 rounded-xl border p-5", className)}>{children}</div>
    </div>
  );
}

function Caption({ children }: { children: ReactNode }) {
  return <span className="font-mono text-[11px] text-muted-foreground">{children}</span>;
}

function BrandingSection() {
  const markRef = useRef<HTMLDivElement>(null);
  const iconRef = useRef<HTMLSpanElement>(null);
  return (
    <Category id="branding" description="The Lightspeed logo, drawn in currentColor, and the realtime starscape from the Lightspeed site.">
      <Specimen title="Logo" note="LightspeedLogo, LightspeedMark" className="gap-8">
        <LightspeedLogo title="Lightspeed" className="h-32 w-auto" />
        <LightspeedMark title="Lightspeed" className="h-24 w-auto" />
        {[48, 32, 24, 16].map((size) => (
          <div key={size} className="grid justify-items-center gap-1.5">
            <LightspeedMark style={{ height: size }} className="w-auto" />
            <Caption>{size}px</Caption>
          </div>
        ))}
        <div className="flex items-center gap-4 rounded-lg bg-primary px-4 py-3 text-primary-foreground">
          <LightspeedMark className="h-8 w-auto" />
          <span className="text-sm font-medium">On primary</span>
        </div>
      </Specimen>
      <Specimen title="Starscape" note="The site's settings over its 887 × 1252 frame; no fade" className="p-0">
        <div className="relative h-72 w-full overflow-hidden rounded-xl bg-black">
          <Starscape className="absolute inset-0" />
        </div>
      </Specimen>
      <NebulaSpecimen />
      <div className="grid gap-5 sm:grid-cols-2">
        <Specimen title="Logo on the starscape" note="anchor, fade 60–480px, transparentBackground" className="p-0">
          <div className="relative grid h-72 w-full place-items-center overflow-hidden rounded-xl">
            <Starscape anchor={markRef} fade={{ start: 60, end: 480 }} transparentBackground className="absolute inset-0 invert dark:invert-0" />
            <div ref={markRef} className="relative">
              <LightspeedMark title="Lightspeed" className="h-16 w-auto" />
            </div>
          </div>
        </Specimen>
        <Specimen
          title="Behind the universe icon"
          note={`As in the sidebar header: fade ${HEADER_STARSCAPE_FADE.start}–${HEADER_STARSCAPE_FADE.end}px`}
          className="p-0"
        >
          <div className="relative h-72 w-full overflow-hidden rounded-xl bg-sidebar text-sidebar-foreground">
            <Starscape anchor={iconRef} fade={HEADER_STARSCAPE_FADE} config={HEADER_STARSCAPE_CONFIG} transparentBackground className={HEADER_STARSCAPE_CLASS} />
            <div className="relative flex h-16 items-center gap-2 px-4">
              <UniverseIcon ref={iconRef} />
              <span className="text-sm font-medium">Universe</span>
            </div>
            <div className="relative grid gap-1 px-4 text-sm text-muted-foreground">
              {["Overview", "Sessions", "Bots"].map((item) => <span key={item} className="py-1.5">{item}</span>)}
            </div>
          </div>
        </Specimen>
      </div>
      <Specimen title="Scale and still frames" note="scale 0.5 and 2; animate off" className="grid gap-3 p-0 sm:grid-cols-3">
        {[
          { label: "scale={0.5}", props: { scale: 0.5 } },
          { label: "scale={2}", props: { scale: 2 } },
          { label: "animate={false}", props: { animate: false } },
        ].map((variant) => (
          <div key={variant.label} className="relative h-40 overflow-hidden rounded-xl bg-black">
            <Starscape className="absolute inset-0" {...variant.props} />
            <span className="absolute bottom-2 left-2 font-mono text-[11px] text-white/70">{variant.label}</span>
          </div>
        ))}
      </Specimen>
    </Category>
  );
}

/// The pages' background on a large surface, as behind the content area.
/// Regenerate draws another seed's nebula; the app keeps its own.
function NebulaSpecimen() {
  const [seed, setSeed] = useState(PAGE_NEBULA_SEED);
  return (
    <Specimen
      title="Page nebula"
      note={`PageNebula seed={${seed}}${seed === PAGE_NEBULA_SEED ? " (the app's)" : ""}, at --page-nebula-opacity`}
      className="p-0"
    >
      <div className="relative isolate h-[32rem] w-full overflow-hidden rounded-xl bg-background">
        <PageNebula seed={seed} className="absolute inset-0 -z-10" />
        <Button
          variant="outline"
          size="sm"
          className="absolute top-3 right-3"
          onClick={() => setSeed(Math.floor(Math.random() * 1_000_000))}
        >
          <RefreshIcon data-icon="inline-start" />
          Regenerate
        </Button>
      </div>
    </Specimen>
  );
}

function TypographySection() {
  const sizes = ["text-xs", "text-sm", "text-base", "text-lg", "text-xl", "text-2xl"] as const;
  const weights = ["font-normal", "font-medium", "font-semibold", "font-bold"] as const;
  return (
    <Category id="typography">
      <Specimen title="Page and section headers" className="block">
        <PageHeader title="Page title" description="A page description explains what lives here." actions={<Button><AddIcon data-icon="inline-start" />Add item</Button>} />
        <SectionHeader title="Section title" description="A section description." actions={<Button variant="outline" size="sm">Action</Button>} />
      </Specimen>
      <Specimen title="Sizes" className="grid gap-2">
        {sizes.map((size) => (
          <div key={size} className="flex items-baseline gap-4">
            <span className="w-20 shrink-0"><Caption>{size}</Caption></span>
            <span className={size}>The quick brown fox jumps over the lazy dog</span>
          </div>
        ))}
      </Specimen>
      <Specimen title="Weights" className="grid gap-2">
        {weights.map((weight) => (
          <div key={weight} className="flex items-baseline gap-4">
            <span className="w-24 shrink-0"><Caption>{weight}</Caption></span>
            <span className={weight}>Sessions, bots and workspaces</span>
          </div>
        ))}
      </Specimen>
      <Specimen title="Text styles" className="grid gap-2">
        <p className="text-sm">Body text in the foreground color.</p>
        <p className="text-sm text-muted-foreground">Muted text for descriptions and hints.</p>
        <p className="text-xs font-medium tracking-wide text-muted-foreground uppercase">Group heading</p>
        <p className="font-mono text-sm">font-mono: session-01HZX4K2 · ~/workspace/src/index.ts</p>
        <p className="text-sm">
          Inline <code className="rounded bg-muted px-1 py-0.5 font-mono text-xs">code</code>, a{" "}
          <a href="#typography" className="underline underline-offset-4 hover:text-primary">link</a>, and{" "}
          <kbd className="rounded border bg-muted px-1 font-mono text-xs">⌘K</kbd>.
        </p>
        <p className="text-sm text-destructive">Destructive text for errors.</p>
      </Specimen>
    </Category>
  );
}

const COLOR_TOKENS = [
  ["background", "foreground"],
  ["card", "card-foreground"],
  ["popover", "popover-foreground"],
  ["primary", "primary-foreground"],
  ["secondary", "secondary-foreground"],
  ["muted", "muted-foreground"],
  ["accent", "accent-foreground"],
  ["destructive", "background"],
  ["sidebar", "sidebar-foreground"],
  ["sidebar-primary", "sidebar-primary-foreground"],
  ["sidebar-accent", "sidebar-accent-foreground"],
] as const;

/// Full class names, so Tailwind's source scan generates them.
const STATUS_COLORS = [
  { name: "success", label: "Success", message: "Deployment finished.", text: "text-success", tint: "border-success/40 bg-success/15", solid: "bg-success" },
  { name: "warning", label: "Warning", message: "Quota at 90%.", text: "text-warning", tint: "border-warning/40 bg-warning/15", solid: "bg-warning" },
  { name: "danger", label: "Danger", message: "Connection lost.", text: "text-danger", tint: "border-danger/40 bg-danger/15", solid: "bg-danger" },
  { name: "info", label: "Info", message: "A new version is out.", text: "text-info", tint: "border-info/40 bg-info/15", solid: "bg-info" },
  {
    name: "destructive",
    label: "Destructive",
    message: "Deletes the workspace.",
    text: "text-destructive",
    tint: "border-destructive/40 bg-destructive/15",
    solid: "bg-destructive",
  },
] as const;

function ColorsSection() {
  return (
    <Category id="colors" description="Theme tokens from index.css; each swatch shows its paired foreground.">
      <Specimen title="Surfaces and text" className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-4">
        {COLOR_TOKENS.map(([surface, text]) => (
          <div key={surface} className="grid gap-1.5">
            <div
              className="flex h-16 items-end rounded-lg p-2 text-sm font-medium ring-1 ring-foreground/10"
              style={{ background: `var(--${surface})`, color: `var(--${text})` }}
            >
              Aa
            </div>
            <Caption>--{surface}</Caption>
          </div>
        ))}
      </Specimen>
      <Specimen title="Status" note="Same in both themes: text, tinted surface, solid fill" className="grid gap-3 sm:grid-cols-2 lg:grid-cols-5">
        {STATUS_COLORS.map((status) => (
          <div key={status.name} className="grid gap-1.5">
            <p className={cn("text-sm font-medium", status.text)}>{status.label}</p>
            <div className={cn("rounded-lg border px-3 py-2 text-sm", status.tint)}>{status.message}</div>
            <div className={cn("rounded-lg px-3 py-2 text-sm font-medium text-black", status.solid)}>{status.label}</div>
            <Caption>--{status.name}</Caption>
          </div>
        ))}
      </Specimen>
      <Specimen title="Lines and charts" className="grid grid-cols-2 gap-3 sm:grid-cols-4 lg:grid-cols-8">
        {["border", "input", "ring", "sidebar-border", "chart-1", "chart-2", "chart-3", "chart-4", "chart-5"].map((token) => (
          <div key={token} className="grid gap-1.5">
            <div className="h-10 rounded-lg ring-1 ring-foreground/10" style={{ background: `var(--${token})` }} />
            <Caption>--{token}</Caption>
          </div>
        ))}
      </Specimen>
      <Specimen title="Radius" note="--radius and its derived steps">
        {["rounded-sm", "rounded-md", "rounded-lg", "rounded-xl", "rounded-2xl", "rounded-4xl"].map((radius) => (
          <div key={radius} className="grid justify-items-center gap-1.5">
            <div className={cn("size-14 border-2 border-primary bg-muted", radius)} />
            <Caption>{radius}</Caption>
          </div>
        ))}
      </Specimen>
      <Specimen title="Shadows">
        {["shadow-xs", "shadow-sm", "shadow-md", "shadow-lg"].map((shadow) => (
          <div key={shadow} className="grid justify-items-center gap-2">
            <div className={cn("size-16 rounded-lg bg-card ring-1 ring-foreground/5", shadow)} />
            <Caption>{shadow}</Caption>
          </div>
        ))}
      </Specimen>
    </Category>
  );
}

const BUTTON_VARIANTS = ["default", "secondary", "outline", "ghost", "destructive", "link"] as const;
const BUTTON_SIZES = ["xs", "sm", "default", "lg"] as const;
const ICON_SIZES = ["icon-xs", "icon-sm", "icon", "icon-lg"] as const;

function ButtonsSection() {
  return (
    <Category id="buttons">
      <Specimen title="Variants">
        {BUTTON_VARIANTS.map((variant) => (
          <Button key={variant} variant={variant}>{variant}</Button>
        ))}
      </Specimen>
      <Specimen title="Sizes" className="grid gap-3">
        {BUTTON_VARIANTS.map((variant) => (
          <div key={variant} className="flex flex-wrap items-center gap-3">
            <span className="w-24"><Caption>{variant}</Caption></span>
            {BUTTON_SIZES.map((size) => (
              <Button key={size} variant={variant} size={size}>Size {size}</Button>
            ))}
          </div>
        ))}
      </Specimen>
      <Specimen title="With icons">
        <Button><AddIcon data-icon="inline-start" />New session</Button>
        <Button variant="outline">Continue<SwitcherIcon data-icon="inline-end" /></Button>
        <Button variant="secondary" size="sm"><CopyIcon data-icon="inline-start" />Copy</Button>
        <Button variant="destructive"><DeleteIcon data-icon="inline-start" />Delete</Button>
      </Specimen>
      <Specimen title="Icon buttons">
        {ICON_SIZES.map((size) => (
          <Button key={size} size={size} variant="outline" aria-label={`Settings ${size}`}><SettingsIcon /></Button>
        ))}
        {ICON_SIZES.map((size) => (
          <Button key={`ghost-${size}`} size={size} variant="ghost" aria-label={`Bell ${size}`}><NotificationIcon /></Button>
        ))}
      </Specimen>
      <Specimen title="States">
        <Button disabled>Disabled</Button>
        <Button variant="outline" disabled>Disabled outline</Button>
        <Button variant="outline" aria-invalid>Invalid</Button>
        <Button variant="outline" aria-expanded>Expanded</Button>
      </Specimen>
    </Category>
  );
}

const TONES: BotTone[] = ["live", "waiting", "idle", "paused", "attention", "closed"];

function BadgesSection() {
  return (
    <Category id="badges">
      <Specimen title="Badge variants">
        {(["default", "secondary", "outline", "destructive", "ghost", "link"] as const).map((variant) => (
          <Badge key={variant} variant={variant}>{variant}</Badge>
        ))}
        <Badge variant="secondary"><BotIcon data-icon="inline-start" />with icon</Badge>
      </Specimen>
      <Specimen title="Info box" note="ui/info-box: info, success, warning, error; with and without a title" className="grid gap-3 sm:grid-cols-2">
        <InfoBox tone="info" title="Heads up">Sessions keep running when you close this tab.</InfoBox>
        <InfoBox tone="success" title="Connected">The environment answered and is ready for commands.</InfoBox>
        <InfoBox tone="warning" title="Demo Mode">Everything here runs in your browser on example data.</InfoBox>
        <InfoBox tone="error" title="Could not save">The provider rejected the key. Check it and try again.</InfoBox>
        <InfoBox tone="info">A note without a title keeps its icon beside the text.</InfoBox>
        <InfoBox tone="error">An error without a title is announced like one with.</InfoBox>
      </Specimen>
      <Specimen title="Info box on a dialog" note="the same notes on the dialog surface (--surface-dialog), as the welcome dialog's Demo Mode info">
        <div className="grid w-full gap-3 rounded-(--surface-dialog-radius) bg-(--surface-dialog) surface-filter-dialog p-6 text-sm text-popover-foreground shadow-(--surface-dialog-shadow) inset-ring-(length:--surface-dialog-border-width) inset-ring-(color:--surface-dialog-border) sm:grid-cols-2">
          <InfoBox tone="info" title="Heads up">Sessions keep running when you close this tab.</InfoBox>
          <InfoBox tone="success" title="Connected">The environment answered and is ready for commands.</InfoBox>
          <InfoBox tone="warning" title="Demo Mode">Everything here runs in your browser on example data.</InfoBox>
          <InfoBox tone="error" title="Could not save">The provider rejected the key. Check it and try again.</InfoBox>
          <InfoBox tone="info">A note without a title keeps its icon beside the text.</InfoBox>
          <InfoBox tone="error">An error without a title is announced like one with.</InfoBox>
        </div>
      </Specimen>
      <Specimen title="Status dots" note="components/bot/status">
        {TONES.map((tone) => (
          <span key={tone} className="flex items-center gap-2 text-sm">
            <StatusDot tone={tone} />
            {tone}
          </span>
        ))}
      </Specimen>
    </Category>
  );
}

function MetaToolbarSection() {
  return (
    <Category
      id="meta"
      description="components/meta-toolbar: an entity's facts given once as items (icon, value, text, tone, tooltip, priority, side, places, click) and drawn by place: a list row and a page header as icon and value, the strip under the header as tags. Short of width, the least important give way: tags shrink to icon and value, then move into +N."
    >
      <Specimen title="Places" note="One session's items in each place, wrapped so all show" className="block">
        <MetaModesExample />
      </Specimen>
      <Specimen title="Session list rows" note="ListRow marks: place list, interactive off inside the row link, colors on highlight, at most half the line" className="block">
        <MetaSessionListExample />
      </Specimen>
      <Specimen title="Session page" note="place header beside the title (model at the end), place strip under it" className="block">
        <MetaSessionPageExample />
      </Specimen>
      <Specimen title="Bot" note="row, header and strip from one list of items" className="block">
        <MetaBotExample />
      </Specimen>
    </Category>
  );
}

function InputsSection() {
  return (
    <Category id="inputs">
      <Specimen title="Text input" className="grid gap-3 sm:grid-cols-2">
        <Input placeholder="Placeholder" aria-label="Placeholder" />
        <Input defaultValue="With a value" aria-label="With a value" />
        <Input placeholder="Disabled" disabled aria-label="Disabled" />
        <Input defaultValue="Invalid value" aria-invalid aria-label="Invalid" />
        <Input type="password" defaultValue="secret-value" aria-label="Password" />
        <Input type="file" aria-label="File" />
      </Specimen>
      <Specimen title="Textarea" className="grid gap-3 sm:grid-cols-2">
        <Textarea placeholder="Write a message…" aria-label="Message" />
        <Textarea defaultValue="Invalid content" aria-invalid aria-label="Invalid message" />
      </Specimen>
      <Specimen title="Input group" className="grid gap-3 sm:grid-cols-2">
        <InputGroup>
          <InputGroupAddon><SearchIcon /></InputGroupAddon>
          <InputGroupInput placeholder="Search sessions" aria-label="Search sessions" />
        </InputGroup>
        <InputGroup>
          <InputGroupInput placeholder="example" aria-label="Subdomain" />
          <InputGroupAddon align="inline-end"><InputGroupText>.lightspeed.dev</InputGroupText></InputGroupAddon>
        </InputGroup>
        <InputGroup>
          <InputGroupAddon><EmailIcon /></InputGroupAddon>
          <InputGroupInput defaultValue="ada@example.com" aria-label="Email" />
          <InputGroupAddon align="inline-end">
            <InputGroupButton aria-label="Copy"><CopyIcon /></InputGroupButton>
          </InputGroupAddon>
        </InputGroup>
        <InputGroup>
          <InputGroupTextarea placeholder="Ask anything…" aria-label="Prompt" />
          <InputGroupAddon align="block-end" className="justify-between">
            <InputGroupText>Markdown supported</InputGroupText>
            <InputGroupButton variant="default" size="xs">Send</InputGroupButton>
          </InputGroupAddon>
        </InputGroup>
      </Specimen>
      <Specimen title="Label">
        <Label htmlFor="overview-label-input">Label</Label>
        <Input id="overview-label-input" className="max-w-xs" placeholder="Labelled input" />
      </Specimen>
    </Category>
  );
}

const MODELS = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "gpt-5", "gemini-3-pro"];

function SelectsSection() {
  const [model, setModel] = useState<string | null>(MODELS[0] ?? null);
  return (
    <Category id="selects">
      <Specimen title="Select" className="grid gap-3 sm:grid-cols-3">
        <Select defaultValue="owner">
          <SelectTrigger aria-label="Role"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="owner">Owner</SelectItem>
            <SelectItem value="editor">Editor</SelectItem>
            <SelectItem value="viewer">Viewer</SelectItem>
          </SelectContent>
        </Select>
        <Select defaultValue="medium">
          <SelectTrigger size="sm" aria-label="Effort"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectGroup>
              <SelectLabel>Effort</SelectLabel>
              <SelectItem value="low">Low</SelectItem>
              <SelectItem value="medium">Medium</SelectItem>
              <SelectItem value="high">High</SelectItem>
            </SelectGroup>
            <SelectSeparator />
            <SelectItem value="none" disabled>None (disabled)</SelectItem>
          </SelectContent>
        </Select>
        <Select defaultValue="a" disabled>
          <SelectTrigger aria-label="Disabled select"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="a">Disabled</SelectItem>
          </SelectContent>
        </Select>
      </Specimen>
      <Specimen title="Combobox" className="grid gap-3 sm:grid-cols-2">
        <Combobox items={MODELS} value={model} onValueChange={setModel}>
          <ComboboxInput placeholder="Search models…" className="w-full" aria-label="Model" />
          <ComboboxContent>
            <ComboboxEmpty>No models found.</ComboboxEmpty>
            <ComboboxList>
              {(item: string) => <ComboboxItem key={item} value={item}>{item}</ComboboxItem>}
            </ComboboxList>
          </ComboboxContent>
        </Combobox>
        <Combobox items={MODELS} defaultValue={MODELS[1]}>
          <ComboboxInput placeholder="Search models…" className="w-full" showClear aria-label="Model with clear" />
          <ComboboxContent>
            <ComboboxEmpty>No models found.</ComboboxEmpty>
            <ComboboxList>
              {(item: string) => <ComboboxItem key={item} value={item}>{item}</ComboboxItem>}
            </ComboboxList>
          </ComboboxContent>
        </Combobox>
      </Specimen>
    </Category>
  );
}

function TogglesSection() {
  return (
    <Category id="toggles">
      <Specimen title="Checkbox">
        <Label className="gap-2"><Checkbox /> Unchecked</Label>
        <Label className="gap-2"><Checkbox defaultChecked /> Checked</Label>
        <Label className="gap-2"><Checkbox disabled /> Disabled</Label>
        <Label className="gap-2"><Checkbox defaultChecked disabled /> Checked, disabled</Label>
        <Label className="gap-2"><Checkbox aria-invalid /> Invalid</Label>
      </Specimen>
      <Specimen title="Switch">
        <Label className="gap-2"><Switch /> Off</Label>
        <Label className="gap-2"><Switch defaultChecked /> On</Label>
        <Label className="gap-2"><Switch disabled /> Disabled</Label>
        <Label className="gap-2"><Switch defaultChecked disabled /> On, disabled</Label>
      </Specimen>
    </Category>
  );
}

function FormsSection() {
  return (
    <Category id="forms">
      <Specimen title="Vertical fields" className="block">
        <FieldGroup className="max-w-md">
          <Field>
            <FieldLabel htmlFor="overview-name">Name</FieldLabel>
            <Input id="overview-name" defaultValue="Release planner" />
            <FieldDescription>Shown in the sidebar and in channel messages.</FieldDescription>
          </Field>
          <Field data-invalid>
            <FieldLabel htmlFor="overview-slug">Slug</FieldLabel>
            <Input id="overview-slug" defaultValue="Release Planner" aria-invalid />
            <FieldError>Use lowercase letters, digits and dashes.</FieldError>
          </Field>
          <Field data-disabled>
            <FieldLabel htmlFor="overview-owner">Owner (disabled)</FieldLabel>
            <Input id="overview-owner" defaultValue="ada@example.com" disabled />
          </Field>
        </FieldGroup>
      </Specimen>
      <Specimen title="Horizontal fields" className="block">
        <FieldGroup className="max-w-md">
          <Field orientation="horizontal">
            <Switch id="overview-notify" defaultChecked />
            <FieldContent>
              <FieldLabel htmlFor="overview-notify">Notify on completion</FieldLabel>
              <FieldDescription>Send a message when a run finishes.</FieldDescription>
            </FieldContent>
          </Field>
          <Field orientation="horizontal">
            <Checkbox id="overview-archive" />
            <FieldLabel htmlFor="overview-archive">Archive finished sessions</FieldLabel>
          </Field>
        </FieldGroup>
      </Specimen>
      <Specimen title="Field set, legend and separator" className="block">
        <FieldSet className="max-w-md">
          <FieldLegend>Limits</FieldLegend>
          <FieldDescription>Bounds for every run this bot starts.</FieldDescription>
          <FieldGroup>
            <Field>
              <FieldLabel htmlFor="overview-turns">Max turns</FieldLabel>
              <Input id="overview-turns" type="number" defaultValue={40} />
            </Field>
            <FieldSeparator>or</FieldSeparator>
            <Field>
              <FieldLabel htmlFor="overview-budget">Daily budget (USD)</FieldLabel>
              <Input id="overview-budget" type="number" defaultValue={25} />
            </Field>
          </FieldGroup>
        </FieldSet>
      </Specimen>
      <Specimen title="Settings disclosure" note="ui/settings-disclosure" className="block">
        <div className="grid max-w-md gap-6">
          <SettingsDisclosure summary="Default limits" action="Customize limits">
            <SettingsGroup title="Run limits" description="Applied to every run.">
              <Field>
                <FieldLabel htmlFor="overview-timeout">Timeout (minutes)</FieldLabel>
                <Input id="overview-timeout" type="number" defaultValue={30} />
              </Field>
            </SettingsGroup>
          </SettingsDisclosure>
          <SettingsDisclosure summary="Model: claude-opus-5-5" action="Change" open>
            <Input defaultValue="claude-opus-5-5" aria-label="Model id" />
          </SettingsDisclosure>
        </div>
      </Specimen>
    </Category>
  );
}

function MenusSection() {
  const [showHidden, setShowHidden] = useState(true);
  const [sort, setSort] = useState("recent");
  return (
    <Category id="menus">
      <Specimen title="Dropdown menu">
        <DropdownMenu>
          <DropdownMenuTrigger render={<Button variant="outline" />}>
            Open menu<SwitcherIcon data-icon="inline-end" />
          </DropdownMenuTrigger>
          <DropdownMenuContent className="w-56" align="start">
            <DropdownMenuGroup>
              <DropdownMenuLabel>Session</DropdownMenuLabel>
              <DropdownMenuItem><CopyIcon />Duplicate<DropdownMenuShortcut>⌘D</DropdownMenuShortcut></DropdownMenuItem>
              <DropdownMenuItem><InboxIcon />Move to inbox</DropdownMenuItem>
              <DropdownMenuItem disabled>Disabled item</DropdownMenuItem>
            </DropdownMenuGroup>
            <DropdownMenuSeparator />
            <DropdownMenuCheckboxItem checked={showHidden} onCheckedChange={setShowHidden}>Show hidden</DropdownMenuCheckboxItem>
            <DropdownMenuSeparator />
            <DropdownMenuGroup>
              <DropdownMenuLabel>Sort by</DropdownMenuLabel>
              <DropdownMenuRadioGroup value={sort} onValueChange={setSort}>
                <DropdownMenuRadioItem value="recent">Most recent</DropdownMenuRadioItem>
                <DropdownMenuRadioItem value="name">Name</DropdownMenuRadioItem>
              </DropdownMenuRadioGroup>
            </DropdownMenuGroup>
            <DropdownMenuSeparator />
            <DropdownMenuSub>
              <DropdownMenuSubTrigger>More</DropdownMenuSubTrigger>
              <DropdownMenuSubContent>
                <DropdownMenuItem>Export transcript</DropdownMenuItem>
                <DropdownMenuItem>Copy link</DropdownMenuItem>
              </DropdownMenuSubContent>
            </DropdownMenuSub>
            <DropdownMenuSeparator />
            <DropdownMenuItem variant="destructive"><DeleteIcon />Delete</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
        <DropdownMenu>
          <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label="More actions" />}>
            <SettingsIcon />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem>Rename</DropdownMenuItem>
            <DropdownMenuItem>Archive</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </Specimen>
    </Category>
  );
}

function OverlaysSection() {
  return (
    <Category id="overlays">
      <Specimen title="Dialog" note="and with the starscape on its backdrop (DialogContent starscape), as a new session's">
        <Dialog>
          <DialogTrigger render={<Button variant="outline" />}>Open dialog</DialogTrigger>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>Rename session</DialogTitle>
              <DialogDescription>The new name shows in the sidebar and in shared links.</DialogDescription>
            </DialogHeader>
            <Input defaultValue="Fix the flaky scheduler test" aria-label="Session name" />
            <DialogFooter>
              <DialogClose render={<Button variant="outline" />}>Cancel</DialogClose>
              <DialogClose render={<Button />}>Save</DialogClose>
            </DialogFooter>
          </DialogContent>
        </Dialog>
        <Dialog>
          <DialogTrigger render={<Button variant="outline" />}>Open with starscape</DialogTrigger>
          <DialogContent starscape>
            <DialogHeader>
              <DialogTitle>New session</DialogTitle>
              <DialogDescription>Start from a named profile or customize an inline setup.</DialogDescription>
            </DialogHeader>
            <Input placeholder="Scratch chat" aria-label="Session name" />
            <DialogFooter>
              <DialogClose render={<Button variant="outline" />}>Cancel</DialogClose>
              <DialogClose render={<Button />}>Create</DialogClose>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      </Specimen>
      <Specimen title="Alert dialog" note="default and sm sizes">
        <AlertDialog>
          <AlertDialogTrigger render={<Button variant="destructive" />}>Delete workspace</AlertDialogTrigger>
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>Delete this workspace?</AlertDialogTitle>
              <AlertDialogDescription>Its files and history are removed. This cannot be undone.</AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>Cancel</AlertDialogCancel>
              <AlertDialogAction variant="destructive">Delete</AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
        <AlertDialog>
          <AlertDialogTrigger render={<Button variant="outline" />}>Small with media</AlertDialogTrigger>
          <AlertDialogContent size="sm">
            <AlertDialogHeader>
              <AlertDialogMedia><NotificationIcon /></AlertDialogMedia>
              <AlertDialogTitle>Enable notifications?</AlertDialogTitle>
              <AlertDialogDescription>Get a message when a bot needs approval.</AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>Not now</AlertDialogCancel>
              <AlertDialogAction>Enable</AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      </Specimen>
      <Specimen title="Sheet" note="all four sides">
        {(["right", "left", "top", "bottom"] as const).map((side) => (
          <Sheet key={side}>
            <SheetTrigger render={<Button variant="outline" />}>{side}</SheetTrigger>
            <SheetContent side={side}>
              <SheetHeader>
                <SheetTitle>Sheet from the {side}</SheetTitle>
                <SheetDescription>Side panels hold secondary detail and settings.</SheetDescription>
              </SheetHeader>
              <SheetFooter><Button>Done</Button></SheetFooter>
            </SheetContent>
          </Sheet>
        ))}
      </Specimen>
      <Specimen title="Popover and tooltip">
        <Popover>
          <PopoverTrigger render={<Button variant="outline" />}>Open popover</PopoverTrigger>
          <PopoverContent className="grid gap-2 p-4">
            <p className="text-sm font-medium">Popover title</p>
            <p className="text-sm text-muted-foreground">Popovers hold short, interactive detail next to their trigger.</p>
            <Button size="sm" className="justify-self-end">Got it</Button>
          </PopoverContent>
        </Popover>
        {(["top", "right", "bottom", "left"] as const).map((side) => (
          <Tooltip key={side}>
            <TooltipTrigger render={<Button variant="ghost" />}>Tooltip {side}</TooltipTrigger>
            <TooltipContent side={side}>Tooltip on the {side}</TooltipContent>
          </Tooltip>
        ))}
      </Specimen>
    </Category>
  );
}

const STEPS = [
  { id: "basics", label: "Basics" },
  { id: "model", label: "Model" },
  { id: "triggers", label: "Triggers" },
  { id: "review", label: "Review" },
] as const;

function NavigationSection() {
  const [step, setStep] = useState<(typeof STEPS)[number]["id"]>("triggers");
  return (
    <Category id="navigation">
      <Specimen title="Tabs" note="default and line" className="grid gap-6">
        <Tabs defaultValue="overview">
          <TabsList>
            <TabsTrigger value="overview">Overview</TabsTrigger>
            <TabsTrigger value="activity">Activity</TabsTrigger>
            <TabsTrigger value="settings">Settings</TabsTrigger>
            <TabsTrigger value="disabled" disabled>Disabled</TabsTrigger>
          </TabsList>
          <TabsContent value="overview" className="text-muted-foreground">Overview panel.</TabsContent>
          <TabsContent value="activity" className="text-muted-foreground">Activity panel.</TabsContent>
          <TabsContent value="settings" className="text-muted-foreground">Settings panel.</TabsContent>
        </Tabs>
        <Tabs defaultValue="chat">
          <TabsList variant="line">
            <TabsTrigger value="chat">Chat</TabsTrigger>
            <TabsTrigger value="activity">Activity</TabsTrigger>
            <TabsTrigger value="files">Files</TabsTrigger>
          </TabsList>
        </Tabs>
      </Specimen>
      <Specimen title="Vertical tabs">
        <Tabs defaultValue="general" orientation="vertical" className="flex-row">
          <TabsList>
            <TabsTrigger value="general">General</TabsTrigger>
            <TabsTrigger value="channels">Channels</TabsTrigger>
            <TabsTrigger value="templates">Templates</TabsTrigger>
          </TabsList>
          <TabsContent value="general" className="px-4 text-muted-foreground">General settings.</TabsContent>
          <TabsContent value="channels" className="px-4 text-muted-foreground">Channel settings.</TabsContent>
          <TabsContent value="templates" className="px-4 text-muted-foreground">Templates.</TabsContent>
        </Tabs>
      </Specimen>
      <Specimen title="Progress steps" note="ui/progress-steps" className="block p-0">
        <ProgressSteps steps={STEPS} current={step} onSelect={setStep} />
      </Specimen>
    </Category>
  );
}

/// Lists: the list pane with nested rows (the tree, status dots as its
/// nodes) and the file list.
function ListsSection() {
  return (
    <Category id="lists">
      <BotListExample />
      <FileListExample />
    </Category>
  );
}

/// A list pane with its header, toolbar, group label and nested rows (the
/// tree, status dots as its nodes), beside a detail pane sharing its bar.
function BotListExample() {
  const [selecting, setSelecting] = useState(false);
  const [picked, setPicked] = useState<Set<string>>(() => new Set(["deploy"]));
  const toggle = (id: string) =>
    setPicked((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  // A tree two levels deep: each row says whether it is the last of its
  // siblings, which ancestor lines run past it, and whether it has children.
  const rows = [
    { id: "planner", title: "Release planner", tone: "live" as BotTone, meta: "2m", line: "Working on #12 · message", tree: { depth: 0, children: true } },
    { id: "deploy", title: "Deploy checks", tone: "idle" as BotTone, meta: "1h", line: "#9 handled · All green", tree: { depth: 1, last: false, lines: [], children: true } },
    { id: "smoke", title: "Smoke test", tone: "idle" as BotTone, meta: "1h", line: "Passed in 42s", tree: { depth: 2, last: false, lines: [true] } },
    { id: "canary", title: "Canary watch", tone: "live" as BotTone, meta: "4m", line: "Watching error rates", tree: { depth: 2, last: true, lines: [true] } },
    { id: "notes", title: "Release notes", tone: "idle" as BotTone, meta: "2h", line: "Drafted 6 entries", tree: { depth: 1, last: true, lines: [] } },
    { id: "triage", title: "Issue triage", tone: "attention" as BotTone, meta: <Badge variant="secondary">3</Badge>, line: "#4 run failed · Provider error", tree: { depth: 0 } },
  ];
  return (
    <Specimen
      title="List pane"
      note="ListPaneHeader, ListPaneToolbar, ListPaneBody, ListGroupLabel, ListRow (nested with tree), ListRowLine; PaneBar sets the header height"
      className="block p-0"
    >
      <div className="flex h-[30rem] overflow-hidden rounded-xl">
        <div className="flex w-80 shrink-0 flex-col border-r">
          <ListPaneHeader
            title="Bots"
            count={rows.length}
            actions={<>
              <Button variant="ghost" size="icon-sm" className={cn(selecting && "text-primary")} onClick={() => setSelecting(!selecting)} aria-label="Select">
                <SelectModeIcon />
              </Button>
              <Button variant="ghost" size="icon-sm" aria-label="New"><AddIcon /></Button>
            </>}
          />
          {selecting && (
            <ListPaneToolbar className="bg-muted/40">
              <span className="text-muted-foreground">{picked.size} selected</span>
            </ListPaneToolbar>
          )}
          <ListPaneBody>
            <ListGroupLabel>Active</ListGroupLabel>
            <ul>
              {rows.map((row) => (
                <ListRow
                  key={row.id}
                  to={`#${row.id}`}
                  active={row.id === "planner"}
                  selection={selecting ? { checked: picked.has(row.id), onToggle: () => toggle(row.id), label: `Select ${row.title}` } : undefined}
                  tree={row.tree}
                  indicator={<StatusDot tone={row.tone} />}
                  title={row.title}
                  meta={row.meta}
                >
                  <ListRowLine tone={row.tone === "attention" ? "destructive" : undefined}>{row.line}</ListRowLine>
                </ListRow>
              ))}
            </ul>
          </ListPaneBody>
        </div>
        <div className="flex min-w-0 flex-1 flex-col">
          <PaneBar as="header">
            <span className="truncate text-sm font-semibold">Detail header</span>
            <span className="text-xs text-muted-foreground">same PaneBar height</span>
          </PaneBar>
          <p className="p-4 text-sm text-muted-foreground">The detail of the selected row.</p>
        </div>
      </div>
    </Specimen>
  );
}


/// A file or folder in the file list example.
interface ExampleFile {
  id: string;
  title: string;
  /** When it last changed. */
  meta: string;
  /** What a folder holds, a file's size and type. */
  line: string;
  /** A folder's contents; a file has none. */
  children?: ExampleFile[];
}

const WORKSPACE_FILES: ExampleFile[] = [
  {
    id: "src", title: "src", meta: "2m", line: "4 items", children: [
      {
        id: "components", title: "components", meta: "2m", line: "3 items", children: [
          { id: "list-pane", title: "list-pane.tsx", meta: "2m", line: "14.2 KB · TypeScript" },
          { id: "file-icon", title: "file-icon.ts", meta: "5m", line: "3.1 KB · TypeScript" },
          { id: "styles", title: "styles.css", meta: "1h", line: "8.7 KB · CSS" },
        ],
      },
      {
        id: "scripts", title: "scripts", meta: "1d", line: "2 items", children: [
          { id: "deploy", title: "deploy.sh", meta: "1d", line: "1.4 KB · Shell" },
          { id: "seed", title: "seed.py", meta: "4d", line: "2.9 KB · Python" },
        ],
      },
      { id: "main", title: "main.rs", meta: "3d", line: "1.1 KB · Rust" },
      { id: "schema", title: "schema.sql", meta: "6d", line: "4.0 KB · SQL" },
    ],
  },
  {
    id: "docs", title: "docs", meta: "1h", line: "6 items", children: [
      { id: "design", title: "design.pdf", meta: "1h", line: "2.4 MB · PDF" },
      { id: "brief", title: "brief.docx", meta: "2d", line: "88 KB · Word" },
      { id: "budget", title: "budget.xlsx", meta: "2d", line: "41 KB · Excel" },
      { id: "pitch", title: "pitch.pptx", meta: "1w", line: "6.1 MB · PowerPoint" },
      { id: "screenshot", title: "screenshot.png", meta: "3h", line: "512 KB · PNG" },
      { id: "walkthrough", title: "walkthrough.mp4", meta: "1w", line: "48 MB · Video" },
    ],
  },
  { id: "dockerfile", title: "Dockerfile", meta: "2w", line: "0.6 KB · Docker" },
  { id: "env", title: ".env.example", meta: "2w", line: "0.3 KB · Settings" },
  { id: "readme", title: "README.md", meta: "5d", line: "6.4 KB · Markdown" },
  { id: "package", title: "package.json", meta: "2w", line: "2.3 KB · JSON" },
  { id: "exports", title: "exports.csv", meta: "3w", line: "1.2 MB · CSV" },
  { id: "backup", title: "backup.tar.gz", meta: "1mo", line: "220 MB · Archive" },
];

/// The rows of a file tree that show: each file and folder, and the contents
/// of the open folders, with its place in the tree (whether it is the last of
/// its siblings, which ancestor lines run past it, whether children follow).
function visibleFiles(files: ExampleFile[], open: Set<string>, depth = 0, lines: boolean[] = []): Array<{ file: ExampleFile; tree: ListRowTree }> {
  return files.flatMap((file, index) => {
    const last = index === files.length - 1;
    const shown = file.children && open.has(file.id) ? file.children : [];
    const tree: ListRowTree = { depth, last, lines, children: shown.length > 0 };
    return [{ file, tree }, ...visibleFiles(shown, open, depth + 1, depth === 0 ? [] : [...lines, !last])];
  });
}

/// A file's or folder's ⋯ menu in the file list example: what can be done
/// to it (nothing happens yet).
function FileActionsMenu({ name, folder }: { name: string; folder: boolean }) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={<Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label={`Actions for ${name}`} title="Actions" />}
      >
        <MoreIcon />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-48">
        {folder && (
          <>
            <DropdownMenuGroup>
              <DropdownMenuItem><NewFileIcon /> New file</DropdownMenuItem>
              <DropdownMenuItem><NewFolderIcon /> New folder</DropdownMenuItem>
            </DropdownMenuGroup>
            <DropdownMenuSeparator />
          </>
        )}
        <DropdownMenuGroup>
          <DropdownMenuItem><EditIcon /> Rename</DropdownMenuItem>
          {!folder && <DropdownMenuItem><CopyIcon /> Duplicate</DropdownMenuItem>}
          <DropdownMenuItem><CopyLinkIcon /> Copy path</DropdownMenuItem>
          <DropdownMenuItem><DownloadIcon /> Download</DropdownMenuItem>
        </DropdownMenuGroup>
        <DropdownMenuSeparator />
        <DropdownMenuItem variant="destructive"><DeleteIcon /> Delete</DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/// The file list: the list pane in files mode, compressed, showing a
/// workspace's folders and files as a tree, each with its icon.
function FileListExample() {
  const [selecting, setSelecting] = useState(false);
  const [picked, setPicked] = useState<Set<string>>(() => new Set(["list-pane"]));
  const toggle = (id: string) =>
    setPicked((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  // Folders open and close; their rows (and tree lines) are built from
  // what is open.
  const [open, setOpen] = useState<Set<string>>(() => new Set(["src", "components", "docs"]));
  const flip = (id: string) =>
    setOpen((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  const rows = visibleFiles(WORKSPACE_FILES, open).map((row, index, all) => ({
    ...row,
    tree: { ...row.tree, nextOpen: all[index + 1]?.tree.children ?? false },
  }));
  return (
    <Specimen
      title="File list"
      note='The list pane in files mode, compressed (ListPaneBody mode="files" compressed): folders open and close (ListRow toggle), each file shows its type (ListRow fileName) and its ⋯ menu on hover (ListRow actions)'
      className="block p-0"
    >
      <div className="flex h-[30rem] overflow-hidden rounded-xl">
        <div className="flex w-80 shrink-0 flex-col border-r">
          <ListPaneHeader
            title="Workspace"
            count={rows.length}
            actions={<>
              <Button variant="ghost" size="icon-sm" className={cn(selecting && "text-primary")} onClick={() => setSelecting(!selecting)} aria-label="Select">
                <SelectModeIcon />
              </Button>
              <Button variant="ghost" size="icon-sm" aria-label="New"><AddIcon /></Button>
            </>}
          />
          {selecting && (
            <ListPaneToolbar className="bg-muted/40">
              <span className="text-muted-foreground">{picked.size} selected</span>
            </ListPaneToolbar>
          )}
          <ListPaneBody mode="files" compressed>
            <ListGroupLabel>Files</ListGroupLabel>
            {/* The rows' top line. */}
            <ul className="border-t">
              {rows.map(({ file, tree }) => {
                // A folder opens and closes; a file opens its detail.
                const folder = file.children !== undefined;
                const isOpen = open.has(file.id);
                return (
                  <ListRow
                    key={file.id}
                    to={folder ? undefined : `#${file.id}`}
                    toggle={folder ? { open: isOpen, onToggle: () => flip(file.id) } : undefined}
                    active={file.id === "list-pane"}
                    selection={selecting ? { checked: picked.has(file.id), onToggle: () => toggle(file.id), label: `Select ${file.title}` } : undefined}
                    tree={tree}
                    fileName={file.title}
                    actions={<FileActionsMenu name={file.title} folder={folder} />}
                    title={file.title}
                    meta={file.meta}
                  >
                    <ListRowLine>{file.line}</ListRowLine>
                  </ListRow>
                );
              })}
            </ul>
          </ListPaneBody>
        </div>
        <div className="flex min-w-0 flex-1 flex-col">
          <PaneBar as="header">
            <span className="truncate text-sm font-semibold">list-pane.tsx</span>
            <span className="text-xs text-muted-foreground">src/components</span>
          </PaneBar>
          <p className="p-4 text-sm text-muted-foreground">The selected file.</p>
        </div>
      </div>
    </Specimen>
  );
}


function CardsSection() {
  return (
    <Category id="cards">
      <Specimen title="Card" note="default and sm" className="grid items-start gap-4 sm:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle>Release planner</CardTitle>
            <CardDescription>Plans and tracks weekly releases.</CardDescription>
            <CardAction><Badge variant="secondary">idle</Badge></CardAction>
          </CardHeader>
          <CardContent>Cards group one resource's summary and actions.</CardContent>
          <CardFooter className="gap-2">
            <Button size="sm">Open</Button>
            <Button size="sm" variant="ghost">Settings</Button>
          </CardFooter>
        </Card>
        <Card size="sm">
          <CardHeader>
            <CardTitle>Small card</CardTitle>
            <CardDescription>Tighter spacing for dense lists.</CardDescription>
          </CardHeader>
          <CardContent>Body content.</CardContent>
        </Card>
      </Specimen>
      <Specimen title="Empty state" note="components/page" className="block">
        <EmptyState icon={InboxIcon} title="No sessions yet">Sessions you start or bots run appear here.</EmptyState>
      </Specimen>
      <Specimen title="Separator">
        <div className="flex h-6 items-center gap-3 text-sm">
          <span>Horizontal below</span>
          <Separator orientation="vertical" />
          <span>Vertical between</span>
        </div>
        <Separator />
      </Specimen>
      <Specimen title="Collapsible" className="block">
        <Collapsible className="grid max-w-md gap-2">
          <CollapsibleTrigger render={<Button variant="outline" size="sm" className="justify-self-start" />}>
            Toggle details<SwitcherIcon data-icon="inline-end" />
          </CollapsibleTrigger>
          <CollapsibleContent className="rounded-md border p-3 text-sm text-muted-foreground">
            Hidden detail that expands in place.
          </CollapsibleContent>
        </Collapsible>
      </Specimen>
    </Category>
  );
}

const ROWS = [
  { name: "acme-web", id: "ws-01HZX4K2Q9", status: "Ready", tone: "idle" as BotTone },
  { name: "release-notes", id: "ws-01HZX4M7T1", status: "Syncing", tone: "live" as BotTone },
  { name: "legacy-api", id: "ws-01HZX4P3B8", status: "Needs attention", tone: "attention" as BotTone },
];

function TablesSection() {
  return (
    <Category id="tables">
      <Specimen title="Table card" note="TableCard, TableTitleCell, IdText, TableActionsCell" className="block p-0 border-0">
        <TableCard>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Workspace</TableHead>
                <TableHead>Status</TableHead>
                <TableHead>Updated</TableHead>
                <TableHead className="text-right">Actions</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {ROWS.map((row) => (
                <TableRow key={row.id}>
                  <TableTitleCell title={row.name} subtitle={row.id} />
                  <TableCell>
                    <span className="flex items-center gap-2"><StatusDot tone={row.tone} />{row.status}</span>
                  </TableCell>
                  <TableCell className="text-muted-foreground">2 hours ago</TableCell>
                  <TableActionsCell>
                    <Button size="xs" variant="ghost">Open</Button>
                  </TableActionsCell>
                </TableRow>
              ))}
            </TableBody>
            <TableCaption>Table caption.</TableCaption>
          </Table>
        </TableCard>
      </Specimen>
      <Specimen title="Id text">
        <IdText className="max-w-48">session-01HZX4K2Q9ZP7Y3M5N8R</IdText>
      </Specimen>
    </Category>
  );
}

function DataSection() {
  return (
    <Category id="data">
      <Specimen title="Avatar" note="sm, default, lg; badge; group">
        {(["sm", "default", "lg"] as const).map((size) => (
          <Avatar key={size} size={size}><AvatarFallback>AL</AvatarFallback></Avatar>
        ))}
        <Avatar><AvatarFallback>GH</AvatarFallback><AvatarBadge className="bg-emerald-500" /></Avatar>
        <AvatarGroup>
          <Avatar><AvatarFallback>AL</AvatarFallback></Avatar>
          <Avatar><AvatarFallback>GH</AvatarFallback></Avatar>
          <Avatar><AvatarFallback>KJ</AvatarFallback></Avatar>
          <AvatarGroupCount>+4</AvatarGroupCount>
        </AvatarGroup>
      </Specimen>
      <Specimen title="Marker" note="default, separator, border" className="grid gap-4">
        <Marker><MarkerIcon><BotIcon /></MarkerIcon><MarkerContent>Planner joined the session</MarkerContent></Marker>
        <Marker variant="separator"><MarkerContent>Today</MarkerContent></Marker>
        <Marker variant="border"><MarkerContent>Context compacted</MarkerContent></Marker>
      </Specimen>
      <Specimen title="Skeleton" className="grid gap-2">
        <div className="flex items-center gap-3">
          <Skeleton className="size-10 rounded-full" />
          <div className="grid flex-1 gap-2">
            <Skeleton className="h-4 w-1/3" />
            <Skeleton className="h-4 w-2/3" />
          </div>
        </div>
      </Specimen>
    </Category>
  );
}

function MessagesSection() {
  const variants = ["default", "secondary", "muted", "tinted", "outline", "ghost", "destructive"] as const;
  return (
    <Category id="messages">
      <Specimen title="Conversation" note="Message and Bubble" className="block">
        <div className="grid max-w-xl gap-4">
          <Message align="end">
            <MessageContent>
              <Bubble><BubbleContent>Can you fix the flaky scheduler test?</BubbleContent></Bubble>
            </MessageContent>
          </Message>
          <Message>
            <MessageContent>
              <MessageHeader>Planner</MessageHeader>
              <BubbleGroup>
                <Bubble variant="secondary"><BubbleContent>Looking at it now.</BubbleContent></Bubble>
                <Bubble variant="secondary">
                  <BubbleContent>The test races the clock; I pinned it and reran it 50 times.</BubbleContent>
                  <BubbleReactions>👍</BubbleReactions>
                </Bubble>
              </BubbleGroup>
              <MessageFooter>09:12</MessageFooter>
            </MessageContent>
          </Message>
        </div>
      </Specimen>
      <Specimen title="Bubble variants" className="grid gap-2">
        {variants.map((variant) => (
          <Bubble key={variant} variant={variant}><BubbleContent>{variant} bubble</BubbleContent></Bubble>
        ))}
      </Specimen>
    </Category>
  );
}

/// The raw source of every module, read for which files import each icon
/// role. Vite inlines it into this development-only chunk.
const GLOBBED_SOURCES = import.meta.glob<string>(
  ["/src/**/*.{ts,tsx}", "!/src/**/*.test.{ts,tsx}", "!/src/**/*.test-double.tsx"],
  { query: "?raw", import: "default", eager: true },
);
// The glob skips the module it is written in, so add this page by hand.
const APP_SOURCES = { ...GLOBBED_SOURCES, "/src/devtools/UiOverviewPage.tsx": OWN_SOURCE };

const ROLE_COMPONENTS = Icons as unknown as Record<string, IconComponent | undefined>;

interface RoleUsage extends IconRole, RoleGlyph {
  files: string[];
}

/// The source of the active library module, found by its `name`.
const LIBRARY_SOURCE =
  Object.entries(APP_SOURCES).find(
    ([file, source]) => file.startsWith("/src/icons/") && source.includes(`name: "${Icons.ICON_LIBRARY}"`),
  )?.[1] ?? "";

function IconsSection() {
  const roles = useMemo(() => {
    const usage = collectRoleUsage(APP_SOURCES);
    const glyphs = libraryGlyphs(LIBRARY_SOURCE);
    return iconRoles(ROLES_SOURCE)
      .filter((role) => ROLE_COMPONENTS[role.role])
      .map((role): RoleUsage => ({
        ...role,
        ...(glyphs.get(role.role) ?? { glyph: "?", library: "?" }),
        files: usage.get(role.role) ?? [],
      }));
  }, []);
  const [query, setQuery] = useState("");
  const needle = query.trim().toLowerCase();
  const shown = roles.filter((role) => `${role.role} ${role.glyph}`.toLowerCase().includes(needle));
  const sections = [...new Set(roles.map((role) => role.section))];
  const glyphs = new Set(roles.map((role) => `${role.library}:${role.glyph}`)).size;
  return (
    <Category
      id="icons"
      description={`The ${roles.length} icon roles declared in src/icons/roles.ts, drawn by the ${Icons.ICON_LIBRARY} library with ${glyphs} distinct icons. Components import roles, never the libraries. Click one to see the files that use it.`}
    >
      <Input
        value={query}
        onChange={(event) => setQuery(event.target.value)}
        placeholder="Filter by role or icon"
        aria-label="Filter icons"
        className="max-w-xs"
      />
      {sections.map((section) => {
        const icons = shown.filter((role) => role.section === section);
        if (icons.length === 0) return null;
        return (
          <Specimen
            key={section}
            title={`${section} (${icons.length})`}
            className="grid grid-cols-[repeat(auto-fill,minmax(9.5rem,1fr))] gap-2"
          >
            {icons.map((role) => (
              <IconTile key={role.role} icon={role} />
            ))}
          </Specimen>
        );
      })}
      {!needle && (
        <Specimen
          title={`Universe icons (${UNIVERSE_ICONS.length})`}
          note={`Illustrations, tinted by the universe's color; ${DEFAULT_UNIVERSE_ICON} is the default`}
          className="grid grid-cols-[repeat(auto-fill,minmax(6rem,1fr))] gap-2"
        >
          {UNIVERSE_ICONS.map((name) => (
            <span key={name} className="grid justify-items-center gap-1.5 rounded-lg border px-2 py-3">
              <UniverseIcon icon={name} className="size-10" />
              <span className="font-mono text-[11px]">{name}</span>
            </span>
          ))}
        </Specimen>
      )}
    </Category>
  );
}

const LIBRARY_LABELS: Record<string, string> = {
  "lucide-react": "lucide",
  "@tabler/icons-react": "tabler",
  "@phosphor-icons/react": "phosphor",
  "@/components/icons/bot": "own",
  "@/components/icons/logos": "own",
};

function IconTile({ icon }: { icon: RoleUsage }) {
  const Icon = ROLE_COMPONENTS[icon.role]!;
  const library = LIBRARY_LABELS[icon.library] ?? icon.library;
  return (
    <Popover>
      <PopoverTrigger
        render={
          <button
            type="button"
            className="grid min-w-0 justify-items-center gap-1.5 rounded-lg border px-2 py-3 text-center outline-none hover:bg-muted/60 focus-visible:ring-3 focus-visible:ring-ring/50"
          />
        }
      >
        <Icon className="size-5" />
        <span className="w-full truncate font-mono text-[11px]" title={icon.role}>{icon.role.replace(/Icon$/, "")}</span>
        <span className="w-full truncate text-[11px] text-muted-foreground" title={`${icon.glyph} from ${icon.library}`}>
          {icon.glyph.replace(/Icon$/, "")} · {library}
        </span>
        <span className={cn("text-[11px]", icon.files.length ? "text-muted-foreground" : "text-warning")}>
          {icon.files.length === 0 ? "unused" : `${icon.files.length} ${icon.files.length === 1 ? "file" : "files"}`}
        </span>
      </PopoverTrigger>
      <PopoverContent className="grid gap-2 p-3">
        <p className="flex items-center gap-2 text-sm font-medium">
          <Icon className="size-4" />
          <span className="font-mono">{icon.role}</span>
        </p>
        <p className="text-xs text-muted-foreground">{icon.glyph} from {icon.library}</p>
        {icon.files.length ? (
          <ul className="grid gap-0.5">
            {icon.files.map((file) => (
              <li key={file} className="truncate font-mono text-[11px] text-muted-foreground" title={file}>{file}</li>
            ))}
          </ul>
        ) : (
          <p className="text-xs text-muted-foreground">No module imports this role.</p>
        )}
      </PopoverContent>
    </Popover>
  );
}

/// Run steps as the transcript names them: one example per verb the runtime
/// projects (see the tool display projection in crates/api-projection).
const STEP_EXAMPLES: Array<{ group: string; entity: string; verb: string; target: string; icon?: IconComponent }> = [
  { group: "explore", entity: "Read tool", verb: "Read", target: "src/scheduler/queue.ts" },
  { group: "explore", entity: "Search tool", verb: "Search", target: "\"flaky retry\"" },
  { group: "explore", entity: "Web fetch", verb: "Fetch", target: "https://docs.example.com/retries" },
  { group: "edit", entity: "Write tool", verb: "Write", target: "src/scheduler/queue.ts" },
  { group: "edit", entity: "Edit tool", verb: "Edit", target: "src/scheduler/queue.test.ts" },
  { group: "execute", entity: "Run tool", verb: "Run", target: "npm test -- queue" },
  { group: "code", entity: "Code mode", verb: "Run code", target: "summarize(results)" },
  { group: "mcp", entity: "MCP tool", verb: "GitHub", target: "create_issue" },
  { group: "agent", entity: "Delegate tool", verb: "Delegate", target: "reviewer" },
  { group: "agent", entity: "Spawn tool", verb: "Spawn", target: "test-writer" },
  { group: "agent", entity: "Await sub-agent", verb: "Await", target: "test-writer", icon: Icons.AwaitToolIcon },
  { group: "bot", entity: "Bot emit", verb: "Emit", target: "to implementer" },
  { group: "bot", entity: "Bot emit to self", verb: "Emit", target: "to self", icon: Icons.EmitToSelfIcon },
  { group: "bot", entity: "Bot brief", verb: "Update brief", target: "Release planner" },
  { group: "message", entity: "Channel send", verb: "Send", target: "#releases" },
  { group: "message", entity: "Channel skip", verb: "Skip", target: "no reply needed" },
  { group: "other", entity: "Other tool", verb: "Run", target: "custom_tool" },
];

interface PlainEntity {
  entity: string;
  icon: IconComponent;
  source: string;
}

/// Session resources as the resource strip and setup editor show them.
const RESOURCE_ENTITIES: PlainEntity[] = [
  { entity: "Environment", icon: Icons.EnvironmentIcon, source: "session/session-resources.tsx" },
  { entity: "Workspace", icon: Icons.WorkspaceIcon, source: "session/session-resources.tsx" },
  { entity: "Tools", icon: Icons.ToolsIcon, source: "session/session-resources.tsx" },
  { entity: "Sub-agents", icon: Icons.SubAgentIcon, source: "session/session-resources.tsx" },
  { entity: "Code mode", icon: Icons.CodeModeIcon, source: "session/session-resources.tsx" },
  { entity: "Web", icon: Icons.WebIcon, source: "session/session-resources.tsx" },
  { entity: "Timers", icon: Icons.TimerIcon, source: "session/session-resources.tsx" },
];

/// Bot trigger kinds, as the trigger picker shows them.
const TRIGGER_ENTITIES: PlainEntity[] = [
  { entity: "Schedule", icon: Icons.ScheduleIcon, source: "bot/triggers.tsx" },
  { entity: "Webhook", icon: Icons.WebhookIcon, source: "bot/triggers.tsx" },
  { entity: "Chat account", icon: Icons.ChatTriggerIcon, source: "bot/triggers.tsx" },
  { entity: "Other bots", icon: Icons.InboxIcon, source: "bot/triggers.tsx" },
  { entity: "Check a URL", icon: Icons.PollIcon, source: "bot/triggers.tsx" },
  { entity: "Run a command", icon: Icons.CommandIcon, source: "bot/triggers.tsx" },
];

/// The universe's sections, straight from the sidebar definition.
const NAV_ENTITIES: PlainEntity[] = UNIVERSE_NAV.flatMap((group) =>
  group.items.map((item) => ({ entity: item.label, icon: item.icon as IconComponent, source: "universe-nav.tsx" })),
);

/// Bot ids for the bot colours specimen: the demo's bots and a few more,
/// spread around the hue wheel.
const SAMPLE_BOTS = ["implementer", "planner", "pr-reviewer", "ci-watch", "intake", "release-scribe", "triage", "digest", "support", "scheduler", "researcher", "notifier"];

function EntitiesSection() {
  const Brain = Icons.ThinkingIcon;
  return (
    <Category
      id="entities"
      description="The things the app shows with an icon, what each looks like in color, and how it appears in use."
    >
      <Specimen title="Run steps" note="Transcript rows: the color comes from the step's tool family" className="block p-0">
        <EntityTable>
          <EntityRow
            entity="Thinking"
            source="session/tool-trace.tsx"
            icon={<Brain className="size-5" />}
            colored={<span className="flex size-8 items-center justify-center rounded-md bg-muted text-muted-foreground"><Brain className="size-4" /></span>}
            preview={<StepRow icon={<Brain />} tone="text-muted-foreground" verb="Thinking" target="Planning the retry fix" />}
          />
          {STEP_EXAMPLES.map((step) => {
            const style = groupStyle(step.group);
            const Icon = step.icon ?? (style.icon as IconComponent);
            return (
              <EntityRow
                key={step.entity}
                entity={step.entity}
                source={`${step.group} family`}
                icon={<Icon className="size-5" />}
                colored={<span className={cn("flex size-8 items-center justify-center rounded-md", style.tile, style.text)}><Icon className="size-4" /></span>}
                preview={<StepRow icon={<Icon />} tone={style.text} verb={step.verb} target={step.target} right="1.2s" />}
              />
            );
          })}
        </EntityTable>
      </Specimen>
      <Specimen
        title="Bot colours"
        note="BotAvatar (components/bot/face): each bot's hue from its id, saturated like the steps (--bot-color-lightness, --bot-color-chroma)"
        className="grid grid-cols-2 gap-x-6 gap-y-3 sm:grid-cols-3 lg:grid-cols-4"
      >
        {SAMPLE_BOTS.map((botId) => (
          <div key={botId} className="flex min-w-0 items-center gap-2.5">
            <BotAvatar botId={botId} size={28} />
            <div className="grid min-w-0">
              <span className="truncate text-sm">{botId}</span>
              <span className="font-mono text-[11px] text-muted-foreground">hue {botHue(botId)}</span>
            </div>
          </div>
        ))}
      </Specimen>
      <Specimen title="Tool families" note={`Folded run strips list them in this order (${GROUP_ORDER.length})`}>
        {GROUP_ORDER.map((group) => {
          const style = groupStyle(group);
          const Icon = style.icon as IconComponent;
          return (
            <span key={group} className={cn("inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-xs font-medium", style.tile, style.text)}>
              <Icon className="size-3.5" />
              {group}
            </span>
          );
        })}
      </Specimen>
      <PlainEntities title="Session resources" note="The resource strip above a session; no color of their own" entities={RESOURCE_ENTITIES} />
      <PlainEntities title="Bot triggers" note="What starts a bot" entities={TRIGGER_ENTITIES} />
      <PlainEntities
        title="Universe sections"
        note="The sidebar"
        entities={NAV_ENTITIES}
      />
    </Category>
  );
}

function PlainEntities({ title, note, entities }: { title: string; note: string; entities: PlainEntity[] }) {
  return (
    <Specimen title={title} note={note} className="block p-0">
      <EntityTable>
        {entities.map(({ entity, icon: Icon, source }) => (
          <EntityRow
            key={entity}
            entity={entity}
            source={source}
            icon={<Icon className="size-5" />}
            colored={<span className="flex size-8 items-center justify-center rounded-md bg-muted text-foreground/75"><Icon className="size-4" /></span>}
            preview={
              <span className="inline-flex items-center gap-1.5 rounded-md border px-2 py-0.5 text-xs">
                <Icon className="size-3.5 text-muted-foreground" />
                {entity}
              </span>
            }
          />
        ))}
      </EntityTable>
    </Specimen>
  );
}

function EntityTable({ children }: { children: ReactNode }) {
  return (
    <div className="grid w-full">
      <div className="grid grid-cols-[minmax(0,11rem)_3rem_3.5rem_minmax(0,1fr)] items-center gap-3 border-b px-4 py-2 text-xs font-medium text-muted-foreground">
        <span>Entity</span>
        <span>Icon</span>
        <span>Color</span>
        <span>In use</span>
      </div>
      {children}
    </div>
  );
}

function EntityRow({ entity, source, icon, colored, preview }: {
  entity: string;
  source: string;
  icon: ReactNode;
  colored: ReactNode;
  preview: ReactNode;
}) {
  return (
    <div className="grid grid-cols-[minmax(0,11rem)_3rem_3.5rem_minmax(0,1fr)] items-center gap-3 border-b px-4 py-2 last:border-b-0">
      <span className="grid min-w-0">
        <span className="truncate text-sm">{entity}</span>
        <span className="truncate" title={source}><Caption>{source}</Caption></span>
      </span>
      <span className="flex justify-center">{icon}</span>
      <span className="flex justify-center">{colored}</span>
      <div className="min-w-0">{preview}</div>
    </div>
  );
}

