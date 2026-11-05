import { useState, type ReactNode } from "react";
import { Bell, Bot, ChevronsUpDown, Copy, Inbox, Mail, Monitor, Moon, Plus, Search, Settings, Sun, Trash2 } from "lucide-react";
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
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { EmptyState, PageHeader, SectionHeader } from "@/components/page";
import { StatusDot, type BotTone } from "@/components/bot/status";
import { useTheme, type Theme } from "@/lib/theme";
import { cn } from "@/lib/utils";

/// Every shared UI component and its variations on one page, grouped by
/// category, for reviewing the design system in both themes. Development
/// builds only: the route is not compiled into production.

const CATEGORIES = [
  { id: "typography", title: "Typography" },
  { id: "colors", title: "Colors" },
  { id: "buttons", title: "Buttons" },
  { id: "badges", title: "Badges & status" },
  { id: "inputs", title: "Inputs" },
  { id: "selects", title: "Selects & comboboxes" },
  { id: "toggles", title: "Checkboxes & switches" },
  { id: "forms", title: "Forms" },
  { id: "menus", title: "Menus" },
  { id: "overlays", title: "Dialogs & overlays" },
  { id: "navigation", title: "Navigation" },
  { id: "cards", title: "Cards & layout" },
  { id: "tables", title: "Tables" },
  { id: "data", title: "Data display" },
  { id: "messages", title: "Messages" },
] as const;

type CategoryId = (typeof CATEGORIES)[number]["id"];

export function UiOverviewPage() {
  return (
    <div className="flex min-h-svh bg-background text-foreground">
      <nav className="sticky top-0 hidden h-svh w-56 shrink-0 overflow-y-auto border-r bg-sidebar p-4 md:block" aria-label="Categories">
        <p className="mb-1 text-sm font-semibold">UI overview</p>
        <p className="mb-4 text-xs text-muted-foreground">Development only</p>
        <ul className="grid gap-0.5">
          {CATEGORIES.map((category) => (
            <li key={category.id}>
              <a
                href={`#${category.id}`}
                className="block rounded-md px-2 py-1 text-sm text-muted-foreground hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
              >
                {category.title}
              </a>
            </li>
          ))}
        </ul>
      </nav>
      <main className="min-w-0 flex-1 px-6 py-8 md:px-10">
        <div className="mx-auto grid max-w-5xl gap-14">
          <div className="flex flex-wrap items-start justify-between gap-4">
            <div className="grid gap-1">
              <h1 className="text-2xl font-semibold tracking-tight">UI overview</h1>
              <p className="text-sm text-muted-foreground">
                The shared components in <code className="font-mono text-xs">src/components/ui</code> and their variations.
              </p>
            </div>
            <ThemeSwitch />
          </div>
          <TypographySection />
          <ColorsSection />
          <ButtonsSection />
          <BadgesSection />
          <InputsSection />
          <SelectsSection />
          <TogglesSection />
          <FormsSection />
          <MenusSection />
          <OverlaysSection />
          <NavigationSection />
          <CardsSection />
          <TablesSection />
          <DataSection />
          <MessagesSection />
        </div>
      </main>
    </div>
  );
}

function ThemeSwitch() {
  const { theme, setTheme } = useTheme();
  const options: Array<{ value: Theme; label: string; icon: ReactNode }> = [
    { value: "light", label: "Light", icon: <Sun /> },
    { value: "dark", label: "Dark", icon: <Moon /> },
    { value: "system", label: "System", icon: <Monitor /> },
  ];
  return (
    <div className="flex gap-1 rounded-lg bg-muted p-1" role="radiogroup" aria-label="Theme">
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
    <section id={id} className="grid scroll-mt-6 gap-5" aria-labelledby={`${id}-title`}>
      <div className="grid gap-1 border-b pb-3">
        <h2 id={`${id}-title`} className="text-lg font-semibold">{title}</h2>
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

function TypographySection() {
  const sizes = ["text-xs", "text-sm", "text-base", "text-lg", "text-xl", "text-2xl"] as const;
  const weights = ["font-normal", "font-medium", "font-semibold", "font-bold"] as const;
  return (
    <Category id="typography">
      <Specimen title="Page and section headers" className="block">
        <PageHeader title="Page title" description="A page description explains what lives here." actions={<Button><Plus data-icon="inline-start" />Add item</Button>} />
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
        <Button><Plus data-icon="inline-start" />New session</Button>
        <Button variant="outline">Continue<ChevronsUpDown data-icon="inline-end" /></Button>
        <Button variant="secondary" size="sm"><Copy data-icon="inline-start" />Copy</Button>
        <Button variant="destructive"><Trash2 data-icon="inline-start" />Delete</Button>
      </Specimen>
      <Specimen title="Icon buttons">
        {ICON_SIZES.map((size) => (
          <Button key={size} size={size} variant="outline" aria-label={`Settings ${size}`}><Settings /></Button>
        ))}
        {ICON_SIZES.map((size) => (
          <Button key={`ghost-${size}`} size={size} variant="ghost" aria-label={`Bell ${size}`}><Bell /></Button>
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
        <Badge variant="secondary"><Bot data-icon="inline-start" />with icon</Badge>
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
          <InputGroupAddon><Search /></InputGroupAddon>
          <InputGroupInput placeholder="Search sessions" aria-label="Search sessions" />
        </InputGroup>
        <InputGroup>
          <InputGroupInput placeholder="example" aria-label="Subdomain" />
          <InputGroupAddon align="inline-end"><InputGroupText>.lightspeed.dev</InputGroupText></InputGroupAddon>
        </InputGroup>
        <InputGroup>
          <InputGroupAddon><Mail /></InputGroupAddon>
          <InputGroupInput defaultValue="ada@example.com" aria-label="Email" />
          <InputGroupAddon align="inline-end">
            <InputGroupButton aria-label="Copy"><Copy /></InputGroupButton>
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
            Open menu<ChevronsUpDown data-icon="inline-end" />
          </DropdownMenuTrigger>
          <DropdownMenuContent className="w-56" align="start">
            <DropdownMenuGroup>
              <DropdownMenuLabel>Session</DropdownMenuLabel>
              <DropdownMenuItem><Copy />Duplicate<DropdownMenuShortcut>⌘D</DropdownMenuShortcut></DropdownMenuItem>
              <DropdownMenuItem><Inbox />Move to inbox</DropdownMenuItem>
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
            <DropdownMenuItem variant="destructive"><Trash2 />Delete</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
        <DropdownMenu>
          <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label="More actions" />}>
            <Settings />
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
      <Specimen title="Dialog">
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
              <AlertDialogMedia><Bell /></AlertDialogMedia>
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
        <EmptyState icon={Inbox} title="No sessions yet">Sessions you start or bots run appear here.</EmptyState>
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
            Toggle details<ChevronsUpDown data-icon="inline-end" />
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
        <Marker><MarkerIcon><Bot /></MarkerIcon><MarkerContent>Planner joined the session</MarkerContent></Marker>
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
