import { platformApi, useAccounts } from "@/lib/platform-api";
import { effectiveFeatures, UNIVERSE_HANDLE_MAX_LENGTH, universeHandleFromName, universeHandleSchema } from "@lightspeed-ai/platform-shared";
import { useRef, useState, type FormEvent, type RefObject } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useDialogWarp } from "@/lib/dialog-warp";
import { useNavigate } from "react-router-dom";
import { AddIcon, SelectedIcon, SwitcherIcon } from "@/icons";
import { UniverseIcon } from "@/components/universe-icon";
import { Starscape } from "@/components/starscape/starscape";
import { useIsDark } from "@/lib/theme";
import { SETTINGS, useSetting } from "@/lib/settings";
import { api, type Universe } from "@/api";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  sidebarMenuButtonVariants,
} from "@/components/ui/sidebar";
import { cn } from "@/lib/utils";
import { memberships, universeHome, useUniverses } from "@/lib/universes";

/// The sky behind the universe icon fades out over this radius, in CSS px.
/// It reaches past the header into the navigation, which the shell keeps
/// stacked above it.
/// The centre is cleared too, so the stars thin out gradually toward the
/// 40px icon: gone within 10px, fully back only by 44px.
export const HEADER_STARSCAPE_FADE = { start: 16, end: 100, inner: { start: 10, end: 44 } };

/// Stars and streaks only: without a sky behind it the nebula reads as a
/// blue tint on the sidebar, and how much shows changes with every seed.
export const HEADER_STARSCAPE_CONFIG = { nebulaAmount: 0 };

/// Covers the fade circle around the icon.
export const HEADER_STARSCAPE_CLASS = "absolute inset-x-0 top-0 h-32";

/// The header's sky, centred on the universe icon and fading out around it,
/// the stars straight on the sidebar. Dark theme only: on a light sidebar
/// the white stars would not show. Not at all with the starscape turned off
/// in Preferences.
export function HeaderStarscape({ iconRef }: { iconRef: RefObject<HTMLElement | null> }) {
  const dark = useIsDark();
  const [shown] = useSetting(SETTINGS.starscape);
  if (!dark || !shown) return null;
  return (
    <Starscape
      anchor={iconRef}
      fade={HEADER_STARSCAPE_FADE}
      config={HEADER_STARSCAPE_CONFIG}
      transparentBackground
      className={HEADER_STARSCAPE_CLASS}
    />
  );
}

/// Top-of-sidebar universe switcher. Lists memberships only; platform
/// admins reach the full inventory via Admin → Universes. Creation lives
/// here (permission-gated) so users find it where they already look.
export function UniverseSwitcher({
  active,
  admin,
}: {
  active: Universe | undefined;
  admin: boolean;
}) {
  const universes = useUniverses();
  const navigate = useNavigate();
  const [createOpen, setCreateOpen] = useState(false);
  const mine = memberships(universes.data);
  const accounts = useAccounts();
  const canCreate = admin || accounts.data?.accounts.some(a => a.enabled && (a.role === "admin" || a.role === "creator"));
  const iconRef = useRef<HTMLSpanElement>(null);

  return (
    <>
      {/* The header's sky: centred on the universe icon, fading out around it. */}
      <HeaderStarscape iconRef={iconRef} />
      <SidebarMenu className="relative">
        <SidebarMenuItem>
          <DropdownMenu>
            <DropdownMenuTrigger
              render={
                <SidebarMenuButton
                  size="lg"
                  className={cn(SWITCHER_BUTTON_CLASS, "data-popup-open:bg-sidebar-accent data-popup-open:text-sidebar-accent-foreground")}
                />
              }
            >
              <SwitcherFace universe={active} iconRef={iconRef} />
              <SwitcherIcon className="ml-auto size-4" />
            </DropdownMenuTrigger>
            <DropdownMenuContent className="min-w-56" align="start">
              <DropdownMenuGroup>
                <DropdownMenuLabel>Universes</DropdownMenuLabel>
              </DropdownMenuGroup>
              {mine.map((universe) => (
                <DropdownMenuItem
                  key={universe.id}
                  onClick={() => navigate(universeHome(universe))}
                >
                  <UniverseIcon icon={universe.icon} iconColor={universe.iconColor} className="size-6 rounded-md" />
                  <span className="truncate">{universe.name}</span>
                  {universe.id === active?.id && <SelectedIcon className="ml-auto size-4" />}
                </DropdownMenuItem>
              ))}
              {mine.length === 0 && (
                <DropdownMenuItem disabled>No universes yet</DropdownMenuItem>
              )}
              {canCreate && (
                <>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem onClick={() => setCreateOpen(true)}>
                    <AddIcon />
                    New universe
                  </DropdownMenuItem>
                </>
              )}
            </DropdownMenuContent>
          </DropdownMenu>
          <NewUniverseDialog open={createOpen} onOpenChange={setCreateOpen} />
        </SidebarMenuItem>
      </SidebarMenu>
    </>
  );
}

/// Half the usual left padding, so the large icon sits near the edge. The
/// icon is taller than the button and reaches into the header's padding, so
/// the button does not clip.
const SWITCHER_BUTTON_CLASS = "h-(--sidebar-header-button-height) overflow-visible pl-1";

/// What the switcher's button shows: the universe's icon and name.
function SwitcherFace({ universe, iconRef }: { universe: Pick<Universe, "name" | "icon" | "iconColor" | "status"> | undefined; iconRef: RefObject<HTMLSpanElement | null> }) {
  return (
    <>
      <UniverseIcon
        ref={iconRef}
        icon={universe?.icon}
        iconColor={universe?.iconColor}
        // The collapsed sidebar's buttons are size-8.
        className="size-11 group-data-[collapsible=icon]:size-8"
      />
      <div className="grid flex-1 text-left leading-tight">
        <span className="truncate text-sm font-medium">
          {universe?.name ?? "Select universe"}
        </span>
        {universe?.status === "archived" && (
          <span className="truncate text-xs text-muted-foreground">archived</span>
        )}
      </div>
    </>
  );
}

/// The switcher as the sidebar header draws it, sky and all, for a preview
/// of a universe's appearance: without its menu or chevron, and inert. It
/// needs no sidebar around it. Sized as the header (sidebar width, pane bar
/// height); `scale` enlarges it, and the space it takes, by a factor.
export function UniverseSwitcherPreview({ universe, scale = 1, className }: {
  universe: Pick<Universe, "name" | "icon" | "iconColor" | "status">;
  scale?: number;
  className?: string;
}) {
  const iconRef = useRef<HTMLSpanElement>(null);
  return (
    <div
      inert
      data-universe-switcher-preview=""
      className={cn("pointer-events-none overflow-hidden rounded-lg border bg-sidebar select-none", className)}
      style={{ width: `calc(${SWITCHER_PREVIEW_WIDTH} * ${scale})`, height: `calc(var(--pane-bar-height) * ${scale})` }}
    >
      <div
        className="relative isolate flex h-(--pane-bar-height) origin-top-left flex-col justify-center p-(--sidebar-header-padding) text-sidebar-foreground"
        style={{ width: SWITCHER_PREVIEW_WIDTH, transform: `scale(${scale})` }}
      >
        <HeaderStarscape iconRef={iconRef} />
        <div className={cn(sidebarMenuButtonVariants({ size: "lg" }), SWITCHER_BUTTON_CLASS, "relative")}>
          <SwitcherFace universe={universe} iconRef={iconRef} />
        </div>
      </div>
    </div>
  );
}

/// The sidebar's width, which the preview takes as the header does.
const SWITCHER_PREVIEW_WIDTH = "16rem";

export function NewUniverseDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const accounts = useAccounts();
  const eligible = (accounts.data?.accounts ?? []).filter(a => a.enabled && (a.role === "admin" || a.role === "creator"));
  const [selectedAccount, setSelectedAccount] = useState("");
  const account = eligible.find(a => a.id === selectedAccount) ?? eligible[0];
  const accountId = account?.id ?? "";
  const mayAppointOwner = account?.role === "admin";
  const owners = useQuery({ queryKey: ["account-members", accountId], enabled: open && !!accountId && mayAppointOwner, queryFn: () => platformApi("platform/accounts/members/list", { accountId }) });
  const me = useQuery({ queryKey: ["creation-identity"], enabled: open, queryFn: () => api<{ user: { id: string } }>("GET", "/api/ui/me") });
  const [ownerId, setOwnerId] = useState("");
  const initialOwnerId = mayAppointOwner
    ? (owners.data?.members.some(m => m.userId === ownerId) ? ownerId : owners.data?.members.find(m => m.userId === me.data?.user.id)?.userId ?? owners.data?.members[0]?.userId ?? "")
    : me.data?.user.id ?? "";
  const universeId = useRef(crypto.randomUUID());
  const [name, setName] = useState("");
  const [handle, setHandle] = useState("");
  const [handleEdited, setHandleEdited] = useState(false);
  const dialogWarp = useDialogWarp(open);

  const create = useMutation({
    mutationFn: async (input: { name: string; handle: string }) =>
      (await platformApi("platform/universes/create", { ...input, accountId, universeId: universeId.current, initialOwner: { kind: "user", id: initialOwnerId } })).universe,
    onSuccess: async (created) => {
      await queryClient.invalidateQueries({ queryKey: ["universes"] });
      // Once the warp has played out: close, and open the new universe.
      dialogWarp.afterWarp(() => {
        onOpenChange(false);
        universeId.current = crypto.randomUUID();
        setName("");
        setHandle("");
        setHandleEdited(false);
        navigate(universeHome({ handle: created.handle, features: effectiveFeatures(created.features) }));
      });
    },
    // Called off: the dialog comes back with the error.
    onError: () => dialogWarp.cancel(),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!accountId || !initialOwnerId || !name.trim() || create.isPending || !universeHandleSchema.safeParse(handle.trim()).success) return;
    // Creating sends the dialog off to warp meanwhile.
    dialogWarp.start();
    create.mutate({ name: name.trim(), handle: handle.trim() });
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[min(92dvh,800px)] overflow-y-auto" starscape warp={dialogWarp.warp}>
        <DialogHeader>
          <DialogTitle>New universe</DialogTitle>
          <DialogDescription>
            Universes start blank — connect chats and configure them afterwards.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <Field><FieldLabel htmlFor="universe-account">Account</FieldLabel>
            <select id="universe-account" className="rounded-md border bg-background p-2" value={accountId} onChange={event => setSelectedAccount(event.target.value)} required>
              {eligible.map(account => <option key={account.id} value={account.id}>{account.name}</option>)}
            </select>
          </Field>
          {mayAppointOwner ? <Field><FieldLabel htmlFor="universe-owner">Initial Owner</FieldLabel><select id="universe-owner" className="rounded-md border bg-background p-2" value={initialOwnerId} onChange={e => setOwnerId(e.target.value)} required>{owners.data?.members.map(m => <option key={m.userId} value={m.userId}>{m.name}</option>)}</select></Field> : <p className="text-sm text-muted-foreground">You will be the initial Owner.</p>}
          {me.error && <p role="alert">{me.error.message}</p>}
          {owners.error && <p role="alert">{owners.error.message}</p>}
          {accounts.error && <p role="alert">{accounts.error.message}</p>}
          <Field>
            <FieldLabel htmlFor="new-universe-name">Name</FieldLabel>
            <Input
              id="new-universe-name"
              value={name}
              onChange={(e) => { setName(e.target.value); if (!handleEdited) setHandle(universeHandleFromName(e.target.value)); }}
              placeholder="My Universe"
              autoFocus
              required
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="new-universe-handle">Handle</FieldLabel>
            <Input id="new-universe-handle" value={handle} required maxLength={UNIVERSE_HANDLE_MAX_LENGTH}
              onChange={(e) => { setHandle(e.target.value); setHandleEdited(true); }} placeholder="my-universe" />
            <p className="text-sm text-muted-foreground">
              Unique in this deployment and used in URLs. Lowercase letters, digits and hyphens.
            </p>
          </Field>
          {create.error && (
            <p className="text-sm text-destructive">{create.error.message}</p>
          )}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={create.isPending || !accountId || !initialOwnerId || !name.trim() || !universeHandleSchema.safeParse(handle.trim()).success}>
              {create.isPending ? "Creating…" : "Create"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
