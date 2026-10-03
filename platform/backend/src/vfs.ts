/// VFS snapshot manifest surgery for the workspace editor. The manifest
/// shape is part of the engine's public contract
/// (`crates/vfs/src/manifest.rs`, schema `lightspeed.vfs.snapshot.v1`):
/// snake_case fields, `kind`-tagged entries, totals validated engine-side
/// on commit — so totals are recomputed here after every change.

export const VFS_SCHEMA_VERSION = "lightspeed.vfs.snapshot.v1";

export interface VfsFile {
  kind: "file";
  blob_ref: string;
  size_bytes: number;
  media_type?: string;
  executable: boolean;
}

export interface VfsDirectory {
  kind: "directory";
  entries: Record<string, VfsEntry>;
}

export type VfsEntry = VfsFile | VfsDirectory;

export interface VfsManifest {
  schema_version: string;
  root: { entries: Record<string, VfsEntry> };
  totals: { files: number; bytes: number };
}

export function emptyManifest(): VfsManifest {
  return { schema_version: VFS_SCHEMA_VERSION, root: { entries: {} }, totals: { files: 0, bytes: 0 } };
}

export class VfsPathError extends Error {}

/// Normalizes and validates a slash-separated file path into segments.
export function pathSegments(path: string): string[] {
  const segments = path.split("/").filter((s) => s.length > 0);
  if (segments.length === 0) {
    throw new VfsPathError("empty path");
  }
  for (const segment of segments) {
    if (segment === "." || segment === "..") {
      throw new VfsPathError(`invalid path segment: ${segment}`);
    }
  }
  return segments;
}

/// Inserts or replaces the file at `path`, creating parent directories.
/// Fails if a parent segment exists as a file, or the target is a directory.
export function setFile(manifest: VfsManifest, path: string, file: VfsFile): void {
  const segments = pathSegments(path);
  const name = segments.pop()!;
  let entries = manifest.root.entries;
  for (const segment of segments) {
    const existing = entries[segment];
    if (existing === undefined) {
      const dir: VfsDirectory = { kind: "directory", entries: {} };
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
export function removeFile(manifest: VfsManifest, path: string): boolean {
  const segments = pathSegments(path);
  const name = segments.pop()!;
  const stack: { entries: Record<string, VfsEntry>; name: string }[] = [];
  let entries = manifest.root.entries;
  for (const segment of segments) {
    const existing = entries[segment];
    if (existing === undefined || existing.kind !== "directory") {
      return false;
    }
    stack.push({ entries, name: segment });
    entries = existing.entries;
  }
  const target = entries[name];
  if (target === undefined || target.kind !== "file") {
    return false;
  }
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

export function recomputeTotals(manifest: VfsManifest): void {
  const totals = { files: 0, bytes: 0 };
  const walk = (entries: Record<string, VfsEntry>) => {
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

/// Defensive parse of an engine manifest (`vfs/snapshots/read` returns it
/// as `unknown`). Structure is trusted beyond the top level — the engine
/// validated it at commit time.
export function asManifest(value: unknown): VfsManifest {
  const manifest = value as VfsManifest;
  if (
    !manifest ||
    manifest.schema_version !== VFS_SCHEMA_VERSION ||
    typeof manifest.root !== "object"
  ) {
    throw new Error("unsupported snapshot manifest shape");
  }
  return manifest;
}
