import { useState, type FormEvent } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import { Check, ChevronsUpDown, Orbit, Plus } from "lucide-react";
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
} from "@/components/ui/sidebar";
import { canManage, memberships, universeHome, useUniverses } from "@/lib/universes";

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

  return (
    <SidebarMenu>
      <SidebarMenuItem>
        <DropdownMenu>
          <DropdownMenuTrigger
            render={
              <SidebarMenuButton
                size="lg"
                className="data-popup-open:bg-sidebar-accent data-popup-open:text-sidebar-accent-foreground"
              />
            }
          >
            <div className="flex size-8 items-center justify-center rounded-lg bg-sidebar-primary text-sidebar-primary-foreground">
              <Orbit className="size-4" />
            </div>
            <div className="grid flex-1 text-left leading-tight">
              <span className="truncate text-sm font-medium">
                {active?.name ?? "Select universe"}
              </span>
              <span className="truncate text-xs text-muted-foreground">
                {active
                  ? active.status === "archived"
                    ? "archived"
                    : "universe"
                  : "Lightspeed"}
              </span>
            </div>
            <ChevronsUpDown className="ml-auto size-4" />
          </DropdownMenuTrigger>
          <DropdownMenuContent className="min-w-56" align="start">
            <DropdownMenuGroup>
              <DropdownMenuLabel>Universes</DropdownMenuLabel>
            </DropdownMenuGroup>
            {mine.map((universe) => (
              <DropdownMenuItem
                key={universe.id}
                onClick={() => navigate(universeHome(universe.slug, canManage(universe, admin)))}
              >
                <span className="truncate">{universe.name}</span>
                {universe.id === active?.id && <Check className="ml-auto size-4" />}
              </DropdownMenuItem>
            ))}
            {mine.length === 0 && (
              <DropdownMenuItem disabled>No universes yet</DropdownMenuItem>
            )}
            {canCreate && (
              <>
                <DropdownMenuSeparator />
                <DropdownMenuItem onClick={() => setCreateOpen(true)}>
                  <Plus />
                  New universe
                </DropdownMenuItem>
              </>
            )}
          </DropdownMenuContent>
        </DropdownMenu>
        <NewUniverseDialog open={createOpen} onOpenChange={setCreateOpen} />
      </SidebarMenuItem>
    </SidebarMenu>
  );
}

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

  const create = useMutation({
    mutationFn: (input: { name: string }) =>
      api<Universe>("POST", "/api/v1/universes", input),
    onSuccess: async (created) => {
      await queryClient.invalidateQueries({ queryKey: ["universes"] });
      onOpenChange(false);
      setName("");
      navigate(universeHome(created.slug, true));
    },
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    create.mutate({ name });
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
              onChange={(e) => setName(e.target.value)}
              placeholder="My Universe"
              autoFocus
              required
            />
          </Field>
          {create.error && (
            <p className="text-sm text-destructive">{create.error.message}</p>
          )}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={create.isPending || !name.trim()}>
              {create.isPending ? "Creating…" : "Create"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
