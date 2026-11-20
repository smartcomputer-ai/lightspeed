import { useEffect, useRef, useState, type FormEvent } from "react";
import { useNavigate } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  UNIVERSE_HANDLE_MAX_LENGTH,
  effectiveFeatures,
  universeHandleFromName,
  universeHandleSchema,
} from "@lightspeed-ai/platform-shared";
import { useDeploymentPermissions } from "@/lib/permissions";
import { organizationLabel } from "@/lib/admin-access";
import { platformApi, useOrganizations } from "@/lib/platform-api";
import { useDialogWarp } from "@/lib/dialog-warp";
import { universeHome } from "@/lib/universes";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { SettingsDisclosure } from "@/components/ui/settings-disclosure";

/// Creates a universe from a name. The handle follows the name and can be
/// changed under Change; the caller becomes its Owner. The organization
/// is a choice only when the caller can create in several.
export function NewUniverseDialog({
  organizationId: fixedOrganizationId,
  defaultOrganizationId,
  open,
  onOpenChange,
}: {
  /// Creation from an organization's administration: no other destination.
  organizationId?: string;
  /// The organization the caller is working in, preferred when it has room.
  defaultOrganizationId?: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const distinguishPersonal = useDeploymentPermissions().can("administerOrganizations");
  const organizations = useOrganizations();
  const eligible = (organizations.data?.organizations ?? []).filter(
    (candidate) => (!fixedOrganizationId || candidate.id === fixedOrganizationId) && candidate.canCreateUniverse,
  );
  const preferred = eligible.find((candidate) => candidate.id === (fixedOrganizationId ?? defaultOrganizationId))?.id ?? eligible[0]?.id ?? "";
  const [selectedOrganization, setSelectedOrganization] = useState(preferred);
  useEffect(() => {
    if (open) setSelectedOrganization(preferred);
  }, [open, preferred]);
  const organization = eligible.find((candidate) => candidate.id === selectedOrganization) ?? eligible[0];
  const organizationId = organization?.id ?? "";
  const organizationName = organization ? organizationLabel(organization, distinguishPersonal) : "";

  // The caller owns what it creates. A Platform Admin outside the
  // organization cannot, so names a member instead; anyone else gives
  // others ownership afterwards, under the universe's Members.
  const outsider = !!organization && organization.role == null;
  const [ownerChoice, setOwnerChoice] = useState<{ organizationId: string; userId: string } | null>(null);
  const owners = useQuery({
    queryKey: ["organization-members", organizationId],
    enabled: open && outsider,
    queryFn: () => platformApi("platform/organizations/members/list", { organizationId }),
  });
  const ownerId = ownerChoice?.organizationId === organizationId ? ownerChoice.userId : "";
  const owner = owners.data?.members.find((candidate) => candidate.userId === ownerId);

  const universeId = useRef(crypto.randomUUID());
  const [name, setName] = useState("");
  const [handle, setHandle] = useState("");
  const [handleEdited, setHandleEdited] = useState(false);
  const handleValid = universeHandleSchema.safeParse(handle.trim()).success;
  const dialogWarp = useDialogWarp(open);

  const create = useMutation({
    mutationFn: async () =>
      (
        await platformApi("platform/universes/create", {
          organizationId,
          universeId: universeId.current,
          name: name.trim(),
          handle: handle.trim(),
          ...(ownerId ? { initialOwner: { kind: "user" as const, id: ownerId } } : {}),
        })
      ).universe,
    onSuccess: async (created) => {
      await queryClient.invalidateQueries({ queryKey: ["universes"] });
      // Once the warp has played out: close, and open the new universe.
      dialogWarp.afterWarp(() => {
        onOpenChange(false);
        universeId.current = crypto.randomUUID();
        setName("");
        setHandle("");
        setHandleEdited(false);
        setOwnerChoice(null);
        navigate(universeHome({ handle: created.handle, features: effectiveFeatures(created.features) }));
      });
    },
    // Called off: the dialog comes back with the error.
    onError: () => dialogWarp.cancel(),
  });
  // A taken handle is fixed where the handle is, so that field opens.
  const handleTaken = !!create.error && /handle/i.test(create.error.message);
  const ready = !!organizationId && !!name.trim() && handleValid && (!outsider || !!ownerId) && !create.isPending;

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!ready) return;
    // Creating sends the dialog off to warp meanwhile.
    dialogWarp.start();
    create.mutate();
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[min(92dvh,800px)] overflow-y-auto" starscape warp={dialogWarp.warp}>
        <DialogHeader>
          <DialogTitle>New universe</DialogTitle>
          <DialogDescription>
            {organization && eligible.length === 1 && <>In {organizationName}. </>}
            It starts empty; add models, bots and chats once it exists.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-5">
          <Field>
            <FieldLabel htmlFor="new-universe-name">Name</FieldLabel>
            <Input
              id="new-universe-name"
              value={name}
              onChange={(event) => {
                setName(event.target.value);
                if (!handleEdited) setHandle(universeHandleFromName(event.target.value));
              }}
              placeholder="Research"
              autoComplete="off"
              autoFocus
              required
            />
          </Field>
          {eligible.length > 1 && (
            <Field>
              <FieldLabel htmlFor="universe-organization">Organization</FieldLabel>
              <Select value={organizationId} onValueChange={(value) => value !== null && setSelectedOrganization(value)}>
                <SelectTrigger id="universe-organization"><SelectValue>{() => organizationName}</SelectValue></SelectTrigger>
                <SelectContent>
                  {eligible.map((candidate) => (
                    <SelectItem key={candidate.id} value={candidate.id}>{organizationLabel(candidate, distinguishPersonal)}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
          )}
          {outsider && (
            <Field>
              <FieldLabel htmlFor="universe-owner">Owner</FieldLabel>
              <Select value={ownerId} onValueChange={(value) => value !== null && setOwnerChoice({ organizationId, userId: value })}>
                <SelectTrigger id="universe-owner">
                  <SelectValue placeholder={owners.isLoading ? "Loading members…" : "Choose a member"}>
                    {() => owner?.name ?? "Choose a member"}
                  </SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {owners.data?.members.map((candidate) => (
                    <SelectItem key={candidate.userId} value={candidate.userId}>{candidate.name}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <FieldDescription>You are not a member of {organizationName}, so a member must own it.</FieldDescription>
              {owners.error && <p role="alert" className="text-sm text-destructive">{owners.error.message}</p>}
            </Field>
          )}
          <SettingsDisclosure
            forceOpen={(!!name.trim() && !handleValid) || handleTaken}
            summary={<>Address <span className="font-mono">/u/{handle.trim() || "…"}</span></>}
            action="Change"
            label="Change the address"
          >
            <Field>
              <FieldLabel htmlFor="new-universe-handle">Handle</FieldLabel>
              <Input
                id="new-universe-handle"
                value={handle}
                required
                maxLength={UNIVERSE_HANDLE_MAX_LENGTH}
                aria-invalid={(!!handle && !handleValid) || handleTaken}
                onChange={(event) => {
                  setHandle(event.target.value);
                  setHandleEdited(true);
                }}
                placeholder="research"
                className="font-mono"
              />
              <FieldDescription>
                The universe's address. Lowercase letters, digits and hyphens, unique in this deployment. It can be changed later.
              </FieldDescription>
            </Field>
          </SettingsDisclosure>
          {organizations.error && <p role="alert" className="text-sm text-destructive">{organizations.error.message}</p>}
          {create.error && <p role="alert" className="text-sm text-destructive">{create.error.message}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>Cancel</Button>
            <Button type="submit" disabled={!ready}>{create.isPending ? "Creating…" : "Create universe"}</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
