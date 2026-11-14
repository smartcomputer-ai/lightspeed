import { slugify, universeSlugSchema } from "@lightspeed-ai/platform-shared";
import { useRef, useState, type FormEvent, type RefObject } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import { AddIcon, SelectedIcon, SwitcherIcon } from "@/icons";
import { UniverseIcon } from "@/components/universe-icon";
import { Starscape } from "@/components/starscape/starscape";
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

/// Covers the fade circle around the icon. The stars are drawn without a sky
/// and inverted in light mode, where they become dark streaks.
export const HEADER_STARSCAPE_CLASS = "absolute inset-x-0 top-0 h-32 invert dark:invert-0";

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
  // Universe creation is platform-admin-only today; the plan is to open
  // this up — flip this gate, nothing else moves.
  const canCreate = admin;
  const iconRef = useRef<HTMLSpanElement>(null);

  return (
    <>
      {/* The header's sky: centred on the universe icon, fading out around it. */}
      <Starscape anchor={iconRef} fade={HEADER_STARSCAPE_FADE} config={HEADER_STARSCAPE_CONFIG} transparentBackground className={HEADER_STARSCAPE_CLASS} />
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
        <Starscape anchor={iconRef} fade={HEADER_STARSCAPE_FADE} config={HEADER_STARSCAPE_CONFIG} transparentBackground className={HEADER_STARSCAPE_CLASS} />
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
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const [slugEdited, setSlugEdited] = useState(false);

  const create = useMutation({
    mutationFn: (input: { name: string; slug: string }) =>
      api<Universe>("POST", "/api/v1/universes", input),
    onSuccess: async (created) => {
      await queryClient.invalidateQueries({ queryKey: ["universes"] });
      onOpenChange(false);
      setName("");
      setSlug("");
      setSlugEdited(false);
      navigate(universeHome(created));
    },
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!universeSlugSchema.safeParse(slug.trim()).success) return;
    create.mutate({ name: name.trim(), slug: slug.trim() });
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New universe</DialogTitle>
          <DialogDescription>
            Universes start blank — connect chats and configure them afterwards.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <Field>
            <FieldLabel htmlFor="new-universe-name">Name</FieldLabel>
            <Input
              id="new-universe-name"
              value={name}
              onChange={(e) => { setName(e.target.value); if (!slugEdited) setSlug(slugify(e.target.value)); }}
              placeholder="My Universe"
              autoFocus
              required
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="new-universe-slug">Slug</FieldLabel>
            <Input id="new-universe-slug" value={slug} required maxLength={128}
              onChange={(e) => { setSlug(e.target.value); setSlugEdited(true); }} placeholder="my-universe" />
            <p className="text-sm text-muted-foreground">Unique in runtime and used in Platform URLs.</p>
          </Field>
          {create.error && (
            <p className="text-sm text-destructive">{create.error.message}</p>
          )}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={create.isPending || !name.trim() || !universeSlugSchema.safeParse(slug.trim()).success}>
              {create.isPending ? "Creating…" : "Create"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
