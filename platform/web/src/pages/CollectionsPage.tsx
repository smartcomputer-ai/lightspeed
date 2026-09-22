import { PrivilegedReadMarker } from "@/components/access/privileged-read";
import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, Folder, Plus } from "lucide-react";
import { Link, NavLink, useNavigate, useParams } from "react-router-dom";
import type {
  CollectionCreateResponse,
  CollectionListResponse,
  CollectionReadResponse,
  CollectionView,
} from "@lightspeed-ai/agent-client";
import { api } from "@/api";
import { useActiveUniverse } from "@/lib/universes";
import { useActionPermissions } from "@/lib/permissions";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Field, FieldLabel } from "@/components/ui/field";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from "@/components/ui/dialog";
import {
  AlertDialog,
  AlertDialogContent,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogCancel,
  AlertDialogAction,
} from "@/components/ui/alert-dialog";
import { LoadingNote, UniverseNotFound } from "@/components/page";
import { ReadError } from "@/components/read-error";
import { AccessButton } from "@/components/access/access-dialog";
import {
  ExecutionLabel,
  RestrictedMarker,
  invalidateAccess,
  resourceHref,
} from "@/components/access/shared";
import {
  CreationAccessFields,
  creationAccessInput,
  defaultCreationAccess,
} from "@/components/access/creation";

export function CollectionsPage() {
  const { universe, slug, isLoading } = useActiveUniverse();
  if (isLoading) return <LoadingNote />;
  if (!universe) return <UniverseNotFound slug={slug} />;
  return <CollectionsWorkspace universeId={universe.id} slug={slug!} />;
}

function CollectionsWorkspace({
  universeId,
  slug,
}: {
  universeId: string;
  slug: string;
}) {
  const { collectionId } = useParams();
  const [createOpen, setCreateOpen] = useState(false);
  const permissions = useActionPermissions(universeId);
  const collections = useQuery({
    queryKey: ["collections", universeId],
    queryFn: () =>
      api<CollectionListResponse>(
        "GET",
        `/api/v1/universes/${universeId}/collections`,
      ),
  });
  return (
    <div className="flex min-h-0 min-w-0 flex-1">
      <aside
        className={cn(
          "w-full shrink-0 flex-col border-r md:flex md:w-72",
          collectionId ? "hidden" : "flex",
        )}
      >
        <div className="flex h-12 shrink-0 items-center gap-2 border-b px-4">
          <h1 className="text-sm font-semibold">Collections</h1>
        <PrivilegedReadMarker privileged={collections.data?.privilegedRead} />
          <span className="text-xs text-muted-foreground">
            {collections.data?.collections.length}
          </span>
          {permissions.can("create_collection") && (
            <Button
              variant="ghost"
              size="icon-sm"
              className="ml-auto"
              aria-label="New collection"
              onClick={() => setCreateOpen(true)}
            >
              <Plus />
            </Button>
          )}
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto">
          {collections.isLoading && (
            <p className="p-4 text-sm text-muted-foreground">Loading…</p>
          )}
          {collections.error && (
            <ReadError error={collections.error} className="p-4" />
          )}
          {collections.data?.collections.length === 0 && (
            <p className="p-4 text-sm text-muted-foreground">
              No collections yet. A collection gives sessions and bots one
              audience and execution identity.
            </p>
          )}
          {collections.data?.collections.map((collection) => (
            <NavLink
              key={collection.collectionId}
              to={`/u/${slug}/collections/${encodeURIComponent(collection.collectionId)}`}
              className={cn(
                "flex items-center gap-2 border-b px-4 py-3 text-sm hover:bg-muted/50",
                collection.collectionId === collectionId && "bg-muted",
              )}
            >
              <Folder className="size-4 text-muted-foreground" />
              <span className="min-w-0 flex-1 truncate font-medium">
                {collection.displayName}
              </span>
              <RestrictedMarker access={collection.access} />
            </NavLink>
          ))}
        </div>
      </aside>
      <section
        className={cn(
          "min-w-0 flex-1 flex-col",
          collectionId ? "flex" : "hidden md:flex",
        )}
      >
        {collectionId ? (
          <CollectionDetail
            key={collectionId}
            universeId={universeId}
            slug={slug}
            collectionId={collectionId}
          />
        ) : (
          <div className="flex flex-1 items-center justify-center p-6 text-sm text-muted-foreground">
            Pick a collection to see its members and access.
          </div>
        )}
      </section>
      <Dialog open={createOpen} onOpenChange={setCreateOpen}>
        <DialogContent className="max-h-[90dvh] overflow-y-auto">
          <DialogHeader>
            <DialogTitle>New collection</DialogTitle>
            <DialogDescription>
              Give related sessions and bots one audience and execution
              identity.
            </DialogDescription>
          </DialogHeader>
          {createOpen && (
            <CreateCollection
              universeId={universeId}
              slug={slug}
              close={() => setCreateOpen(false)}
            />
          )}
        </DialogContent>
      </Dialog>
    </div>
  );
}

function CreateCollection({
  universeId,
  slug,
  close,
}: {
  universeId: string;
  slug: string;
  close: () => void;
}) {
  const [name, setName] = useState("");
  const [access, setAccess] = useState(defaultCreationAccess);
  const client = useQueryClient();
  const navigate = useNavigate();
  const create = useMutation({
    mutationFn: () =>
      api<CollectionCreateResponse>(
        "POST",
        `/api/v1/universes/${universeId}/collections`,
        { displayName: name.trim(), ...creationAccessInput(access) },
      ),
    onSuccess: async ({ collection }) => {
      await invalidateAccess(client, universeId);
      close();
      navigate(
        `/u/${slug}/collections/${encodeURIComponent(collection.collectionId)}`,
      );
    },
  });
  return (
    <form
      className="grid gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        create.mutate();
      }}
    >
      <Field>
        <FieldLabel htmlFor="collection-name">Name</FieldLabel>
        <Input
          id="collection-name"
          autoFocus
          value={name}
          onChange={(e) => setName(e.target.value)}
          maxLength={200}
          required
        />
      </Field>
      <CreationAccessFields
        universeId={universeId}
        value={access}
        onChange={setAccess}
        allowCollection={false}
      />
      {create.error && (
        <p role="alert" className="text-sm text-destructive">
          {create.error.message}
        </p>
      )}
      <DialogFooter>
        <Button
          type="button"
          variant="outline"
          onClick={close}
          disabled={create.isPending}
        >
          Cancel
        </Button>
        <Button disabled={!name.trim() || create.isPending}>
          {create.isPending ? "Creating…" : "Create"}
        </Button>
      </DialogFooter>
    </form>
  );
}

function CollectionDetail({
  universeId,
  slug,
  collectionId,
}: {
  universeId: string;
  slug: string;
  collectionId: string;
}) {
  const detail = useQuery({
    queryKey: ["collection", universeId, collectionId],
    queryFn: () =>
      api<CollectionReadResponse>(
        "GET",
        `/api/v1/universes/${universeId}/collections/${encodeURIComponent(collectionId)}`,
      ),
  });
  const target = { kind: "collection" as const, id: collectionId };
  const permissions = useActionPermissions(universeId, [target]);
  const client = useQueryClient();
  const navigate = useNavigate();
  const [deleteOpen, setDeleteOpen] = useState(false);
  const remove = useMutation({
    mutationFn: () =>
      api(
        "DELETE",
        `/api/v1/universes/${universeId}/collections/${encodeURIComponent(collectionId)}`,
      ),
    onSuccess: async () => {
      await invalidateAccess(client, universeId);
      navigate(`/u/${slug}/collections`);
    },
  });
  if (detail.error) return <ReadError error={detail.error} className="p-6" />;
  if (!detail.data)
    return (
      <div className="p-6">
        <LoadingNote />
      </div>
    );
  const { collection, members } = detail.data;
  const canCreate = permissions.can("control_session", target);
  return (
    <>
      <header className="flex h-12 shrink-0 items-center gap-2 border-b px-4">
        <Link
          to={`/u/${slug}/collections`}
          className="md:hidden"
          aria-label="Back to collections"
        >
          <ArrowLeft className="size-4" />
        </Link>
        <h1 className="min-w-0 flex-1 truncate text-sm font-semibold">
          {collection.displayName}
        </h1>
        <PrivilegedReadMarker privileged={detail.data.privilegedRead} />
        <AccessButton
          universeId={universeId}
          slug={slug}
          resource={target}
          access={collection.access}
        />
      </header>
      <div className="grid content-start gap-6 overflow-y-auto p-6">
        <ExecutionLabel access={collection.access} universeId={universeId} />
        {permissions.can("manage_collection", target) && (
          <RenameCollection
            key={collection.revision}
            universeId={universeId}
            collection={collection}
          />
        )}
        <section className="grid gap-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <h2 className="text-sm font-medium">
              Sessions and bots{" "}
              <span className="text-muted-foreground">{members.length}</span>
            </h2>
            {canCreate && (
              <div className="flex gap-2">
                {permissions.can("create_session") && (
                  <Button
                    variant="outline"
                    size="sm"
                    render={
                      <Link
                        to={`/u/${slug}/sessions?new=session&collection=${encodeURIComponent(collectionId)}`}
                      />
                    }
                  >
                    New session
                  </Button>
                )}
                {permissions.can("create_bot") && (
                  <Button
                    variant="outline"
                    size="sm"
                    render={
                      <Link
                        to={`/u/${slug}/bots?new=bot&collection=${encodeURIComponent(collectionId)}`}
                      />
                    }
                  >
                    New bot
                  </Button>
                )}
              </div>
            )}
          </div>
          <p className="text-sm text-muted-foreground">
            Every member shares this collection's access and execution. Existing
            resources cannot be moved into it.
          </p>
          {members.length ? (
            <ul className="divide-y rounded-lg border">
              {members.map((member) => (
                <li key={`${member.kind}:${member.id}`}>
                  <Link
                    to={resourceHref(slug, member)}
                    className="flex items-center gap-3 px-4 py-3 text-sm hover:bg-muted/50"
                  >
                    <span className="min-w-0 flex-1 truncate">{member.id}</span>
                    <span className="text-xs text-muted-foreground">
                      {member.kind}
                    </span>
                  </Link>
                </li>
              ))}
            </ul>
          ) : (
            <p className="rounded-lg border border-dashed p-6 text-sm text-muted-foreground">
              No sessions or bots in this collection yet.
            </p>
          )}
        </section>
        {permissions.can("delete_collection", target) && (
          <div className="grid justify-items-start gap-2">
            <Button
              variant="outline"
              className="text-destructive"
              disabled={members.length > 0}
              onClick={() => setDeleteOpen(true)}
            >
              Delete collection
            </Button>
            {members.length > 0 && (
              <p className="text-xs text-muted-foreground">
                Delete its members under their own permissions before deleting
                this collection.
              </p>
            )}
          </div>
        )}
      </div>
      <AlertDialog open={deleteOpen} onOpenChange={setDeleteOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              Delete {collection.displayName}?
            </AlertDialogTitle>
            <AlertDialogDescription>
              This empty collection and its sharing policy will be permanently
              removed.
            </AlertDialogDescription>
          </AlertDialogHeader>
          {remove.error && (
            <p role="alert" className="text-sm text-destructive">
              {remove.error.message}
            </p>
          )}
          <AlertDialogFooter>
            <AlertDialogCancel disabled={remove.isPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              disabled={remove.isPending}
              onClick={(e) => {
                e.preventDefault();
                remove.mutate();
              }}
            >
              Delete
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}

function RenameCollection({
  universeId,
  collection,
}: {
  universeId: string;
  collection: CollectionView;
}) {
  const [name, setName] = useState(collection.displayName);
  const client = useQueryClient();
  const save = useMutation({
    mutationFn: () =>
      api(
        "PUT",
        `/api/v1/universes/${universeId}/collections/${encodeURIComponent(collection.collectionId)}`,
        { displayName: name.trim(), expectedRevision: collection.revision },
      ),
    onSuccess: () => invalidateAccess(client, universeId),
  });
  return (
    <form
      className="grid max-w-lg gap-2"
      onSubmit={(e: FormEvent) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <FieldLabel htmlFor="collection-display-name">Name</FieldLabel>
      <div className="flex gap-2">
        <Input
          id="collection-display-name"
          value={name}
          maxLength={200}
          onChange={(e) => setName(e.target.value)}
        />
        <Button
          variant="outline"
          disabled={
            !name.trim() ||
            name.trim() === collection.displayName ||
            save.isPending
          }
        >
          Save
        </Button>
      </div>
      {save.error && (
        <p role="alert" className="text-sm text-destructive">
          {save.error.message}
        </p>
      )}
    </form>
  );
}
