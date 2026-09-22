/// Workspaces and blobs: the file browser's rows, trees, and saves. The
/// manifest surgery is a port of the server's `vfs.ts` against the same
/// engine manifest shape (snake_case, kind-tagged, totals recomputed after
/// every change). A "commit" here is a fresh snapshot ref plus a revision
/// bump on the workspace row; the head manifest lives on the record.
import { Hono } from "hono";
import type { VfsDirEntry, VfsFileEntry, VfsTreeEntry, WorkspaceRow, WorkspaceTree } from "@/api";
import { base64ToBytes, type DemoStore, type WorkspaceRecord } from "../store";
import { badRequest, conflict, notFound, readBody, universeFor } from "./common";

export type Manifest = WorkspaceTree["manifest"];

export const VFS_SCHEMA_VERSION = "lightspeed.vfs.snapshot.v1";

const WORKSPACE_ID = /^[a-z0-9][a-z0-9._-]*$/;

export function workspaceRoutes(store: DemoStore): Hono {
  const app = new Hono();

  app.get("/:id/workspaces", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const rows = [...universe.workspaces.values()]
      .map((record) => record.row)
      .sort((a, b) => b.updatedAtMs - a.updatedAtMs);
    return c.json(rows);
  });

  /// Starts from the empty snapshot at revision 0 (engine truth: the first
  /// save expects 0). The id comes from the body, else the display name,
  /// else a mint.
  app.post("/:id/workspaces", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const body = await readBody<{ workspaceId?: unknown; displayName?: unknown }>(c);
    const displayName = typeof body.displayName === "string" ? body.displayName.trim() || null : null;
    const workspaceId =
      (typeof body.workspaceId === "string" && body.workspaceId.trim()) ||
      (displayName ? slugify(displayName) : store.nextId("ws"));
    if (!WORKSPACE_ID.test(workspaceId) || workspaceId.length > 80) {
      return badRequest(c, "workspaceId must be lowercase letters, digits, '.', '_' or '-'");
    }
    if (universe.workspaces.has(workspaceId)) {
      return conflict(c, `engine conflict: workspace exists: ${workspaceId}`);
    }
    const now = Date.now();
    const row: WorkspaceRow = {
      workspaceId,
      displayName,
      headSnapshotRef: store.nextId("snap"),
      revision: 0,
      files: 0,
      bytes: 0,
      createdAtMs: now,
      updatedAtMs: now,
    };
    universe.workspaces.set(workspaceId, { row, manifest: emptyManifest() });
    return c.json(row, 201);
  });

  app.get("/:id/workspaces/:workspaceId/tree", (c) => {
    const universe = universeFor(store, c);
    const record = universe?.workspaces.get(c.req.param("workspaceId"));
    if (!record) return notFound(c, "not found in engine");
    const tree: WorkspaceTree = { workspace: record.row, manifest: record.manifest };
    return c.json(tree);
  });

  /// Write a file: store the blob, graft it into a copy of the head
  /// manifest, commit. A stale `expectedRevision` is a 409, not a clobber.
  app.put("/:id/workspaces/:workspaceId/files/:path{.+}", async (c) => {
    const universe = universeFor(store, c);
    const record = universe?.workspaces.get(c.req.param("workspaceId"));
    if (!record) return notFound(c, "not found in engine");
    const body = await readBody<{
      contentText?: unknown;
      contentBase64?: unknown;
      mediaType?: unknown;
      expectedRevision?: unknown;
    }>(c);
    const hasText = typeof body.contentText === "string";
    const hasBase64 = typeof body.contentBase64 === "string";
    if (hasText === hasBase64) {
      return badRequest(c, "exactly one of contentText or contentBase64 is required");
    }
    const expectedRevision = revisionOf(body.expectedRevision);
    if (expectedRevision === null) return badRequest(c, "expectedRevision is required");
    if (record.row.revision !== expectedRevision) {
      return conflict(c, "workspace changed since it was loaded — reload and retry");
    }
    const bytes =
      typeof body.contentBase64 === "string"
        ? base64ToBytes(body.contentBase64)
        : new TextEncoder().encode(typeof body.contentText === "string" ? body.contentText : "");
    const blobRef = store.putBytes(bytes);
    const manifest = structuredClone(record.manifest);
    try {
      setFile(manifest, c.req.param("path"), {
        kind: "file",
        blob_ref: blobRef,
        size_bytes: bytes.length,
        ...(typeof body.mediaType === "string" && body.mediaType
          ? { media_type: body.mediaType }
          : {}),
        executable: false,
      });
    } catch (error) {
      if (error instanceof VfsPathError) return badRequest(c, error.message);
      throw error;
    }
    return c.json({ workspace: commitHead(store, record, manifest) });
  });

  app.delete("/:id/workspaces/:workspaceId/files/:path{.+}", (c) => {
    const universe = universeFor(store, c);
    const record = universe?.workspaces.get(c.req.param("workspaceId"));
    if (!record) return notFound(c, "not found in engine");
    const expectedRevision = revisionOf(c.req.query("expectedRevision"));
    if (expectedRevision === null) {
      return badRequest(c, "expectedRevision query parameter is required");
    }
    if (record.row.revision !== expectedRevision) {
      return conflict(c, "workspace changed since it was loaded — reload and retry");
    }
    const manifest = structuredClone(record.manifest);
    let removed: boolean;
    try {
      removed = removeFile(manifest, c.req.param("path"));
    } catch (error) {
      if (error instanceof VfsPathError) return badRequest(c, error.message);
      throw error;
    }
    if (!removed) return notFound(c, "file not found");
    return c.json({ workspace: commitHead(store, record, manifest) });
  });

  app.get("/:id/workspaces/:workspaceId/files/:path{.+}", (c) => {
    const universe = universeFor(store, c);
    const workspace = universe?.workspaces.get(c.req.param("workspaceId"));
    if (!workspace) return notFound(c);
    let entries = workspace.manifest.root.entries;
    const parts = c.req.param("path").split("/").filter(Boolean);
    for (let i = 0; i < parts.length; i++) {
      const entry = entries[parts[i]!];
      if (!entry) return notFound(c);
      if (entry.kind === "file") {
        const blob = i === parts.length - 1 ? store.blobs.get(entry.blob_ref) : undefined;
        return blob ? c.json(blob) : notFound(c);
      }
      entries = entry.entries;
    }
    return badRequest(c, "Path is not a file");
  });

  app.get("/:id/blobs/:blobRef", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const blob = store.blobs.get(c.req.param("blobRef"));
    return blob ? c.json(blob) : notFound(c, "not found in engine");
  });

  return app;
}

// ---------------------------------------------------------------------------
// Manifest surgery (port of platform/server/src/vfs.ts)
// ---------------------------------------------------------------------------

export function emptyManifest(): Manifest {
  return { schema_version: VFS_SCHEMA_VERSION, root: { entries: {} }, totals: { files: 0, bytes: 0 } };
}

export class VfsPathError extends Error {}

/// Normalizes and validates a slash-separated file path into segments.
function pathSegments(path: string): string[] {
  const segments = path.split("/").filter((segment) => segment.length > 0);
  if (segments.length === 0) throw new VfsPathError("empty path");
  for (const segment of segments) {
    if (segment === "." || segment === "..") {
      throw new VfsPathError(`invalid path segment: ${segment}`);
    }
  }
  return segments;
}

/// Inserts or replaces the file at `path`, creating parent directories.
/// Fails if a parent segment exists as a file, or the target is a directory.
export function setFile(manifest: Manifest, path: string, file: VfsFileEntry): void {
  const segments = pathSegments(path);
  const name = segments.pop()!;
  let entries = manifest.root.entries;
  for (const segment of segments) {
    const existing = entries[segment];
    if (existing === undefined) {
      const dir: VfsDirEntry = { kind: "directory", entries: {} };
      entries[segment] = dir;
      entries = dir.entries;
    } else if (existing.kind === "directory") {
      entries = existing.entries;
    } else {
      throw new VfsPathError(`path conflicts with an existing file: ${segment}`);
    }
  }
  const target = entries[name];
  if (target !== undefined && target.kind === "directory") {
    throw new VfsPathError(`path is a directory: ${path}`);
  }
  entries[name] = file;
  recomputeTotals(manifest);
}

/// Removes the file at `path` and prunes directories left empty.
/// Returns false when the path does not exist as a file.
export function removeFile(manifest: Manifest, path: string): boolean {
  const segments = pathSegments(path);
  const name = segments.pop()!;
  const stack: { entries: Record<string, VfsTreeEntry>; name: string }[] = [];
  let entries = manifest.root.entries;
  for (const segment of segments) {
    const existing = entries[segment];
    if (existing === undefined || existing.kind !== "directory") return false;
    stack.push({ entries, name: segment });
    entries = existing.entries;
  }
  const target = entries[name];
  if (target === undefined || target.kind !== "file") return false;
  delete entries[name];
  for (let i = stack.length - 1; i >= 0; i--) {
    const { entries: parent, name: dirName } = stack[i]!;
    const dir = parent[dirName];
    if (dir?.kind === "directory" && Object.keys(dir.entries).length === 0) {
      delete parent[dirName];
    } else {
      break;
    }
  }
  recomputeTotals(manifest);
  return true;
}

function recomputeTotals(manifest: Manifest): void {
  const totals = { files: 0, bytes: 0 };
  const walk = (entries: Record<string, VfsTreeEntry>) => {
    for (const entry of Object.values(entries)) {
      if (entry.kind === "file") {
        totals.files += 1;
        totals.bytes += entry.size_bytes;
      } else {
        walk(entry.entries);
      }
    }
  };
  walk(manifest.root.entries);
  manifest.totals = totals;
}

/// Advances the head: new snapshot ref, revision + 1, totals from the
/// manifest — what `vfs/snapshots/commit` + `vfs/workspaces/update` do.
function commitHead(store: DemoStore, record: WorkspaceRecord, manifest: Manifest): WorkspaceRow {
  record.manifest = manifest;
  record.row.headSnapshotRef = store.nextId("snap");
  record.row.revision += 1;
  record.row.files = manifest.totals.files;
  record.row.bytes = manifest.totals.bytes;
  record.row.updatedAtMs = Date.now();
  return record.row;
}

/// A non-negative integer from a JSON number or a query string; null otherwise.
function revisionOf(value: unknown): number | null {
  const parsed = typeof value === "number" ? value : typeof value === "string" ? Number(value) : NaN;
  return Number.isInteger(parsed) && parsed >= 0 ? parsed : null;
}

function slugify(name: string): string {
  return (
    name
      .toLowerCase()
      .normalize("NFKD")
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 60) || "workspace"
  );
}
