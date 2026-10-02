import { Zip, ZipDeflate } from "fflate";
import { z } from "zod";

export const MAX_WORKSPACE_UPLOAD_BYTES = 32 * 1024 * 1024;
export const MAX_WORKSPACE_UPLOAD_ENTRIES = 10_000;
export const MAX_WORKSPACE_UPLOAD_BODY_BYTES = 48 * 1024 * 1024;

export function validWorkspaceTransferPath(path: string): boolean {
  return (
    path.length > 0 &&
    !/[\\\x00-\x1f]/.test(path) &&
    path
      .split("/")
      .every((part) => part !== "" && part !== "." && part !== "..")
  );
}

const pathSchema = z
  .string()
  .max(4096)
  .refine(validWorkspaceTransferPath, "Invalid relative path");
export const workspaceEntryDeleteSchema = z.object({
  path: pathSchema,
  expectedRevision: z.number().int().nonnegative(),
});
export const workspaceEntryRenameSchema = workspaceEntryDeleteSchema.extend({
  name: pathSchema.refine(
    (name) => !name.includes("/"),
    "Enter a single file or folder name",
  ),
});
const fileSchema = z.object({
  kind: z.literal("file"),
  path: pathSchema,
  contentBase64: z
    .string()
    .max(Math.ceil(MAX_WORKSPACE_UPLOAD_BYTES / 3) * 4)
    .refine((value) => {
      const padding = value.endsWith("==") ? 2 : value.endsWith("=") ? 1 : 0;
      return (
        value.length % 4 === 0 &&
        !/[^A-Za-z0-9+/]/.test(value.slice(0, value.length - padding))
      );
    }, "Invalid base64 content"),
  mediaType: z.string().max(255).optional(),
});
export const workspaceUploadSchema = z
  .object({
    expectedRevision: z.number().int().nonnegative(),
    replace: z.boolean().default(false),
    entries: z
      .array(
        z.discriminatedUnion("kind", [
          fileSchema,
          z.object({ kind: z.literal("directory"), path: pathSchema }),
        ]),
      )
      .min(1)
      .max(MAX_WORKSPACE_UPLOAD_ENTRIES),
  })
  .superRefine((value, ctx) => {
    let bytes = 0;
    const paths = new Set<string>();
    for (const entry of value.entries) {
      if (paths.has(entry.path))
        ctx.addIssue({
          code: "custom",
          message: `Duplicate path: ${entry.path}`,
        });
      paths.add(entry.path);
      if (entry.kind === "file") bytes += base64Size(entry.contentBase64);
    }
    if (bytes > MAX_WORKSPACE_UPLOAD_BYTES)
      ctx.addIssue({ code: "custom", message: "Upload exceeds 32 MiB" });
  });
export type WorkspaceUpload = z.infer<typeof workspaceUploadSchema>;

function base64Size(value: string): number {
  return (
    (value.length / 4) * 3 -
    (value.endsWith("==") ? 2 : value.endsWith("=") ? 1 : 0)
  );
}

interface TransferFile {
  kind: "file";
  blob_ref: string;
  size_bytes: number;
  media_type?: string;
  executable: boolean;
}
interface TransferDirectory {
  kind: "directory";
  entries: Record<string, TransferEntry>;
}
type TransferEntry = TransferFile | TransferDirectory;
interface TransferManifest {
  root: { entries: Record<string, TransferEntry> };
  totals: { files: number; bytes: number };
}

export class WorkspaceTransferError extends Error {
  constructor(
    message: string,
    readonly status: 400 | 404 | 409,
  ) {
    super(message);
  }
}

function own(
  entries: Record<string, TransferEntry>,
  name: string,
): TransferEntry | undefined {
  return Object.hasOwn(entries, name) ? entries[name] : undefined;
}
function assign(
  entries: Record<string, TransferEntry>,
  name: string,
  entry: TransferEntry,
) {
  Object.defineProperty(entries, name, {
    value: entry,
    enumerable: true,
    writable: true,
    configurable: true,
  });
}

export function workspaceUploadConflicts(
  manifest: TransferManifest,
  entries: Array<{ kind: "file" | "directory"; path: string }>,
): string[] {
  const conflicts: string[] = [];
  for (const input of entries) {
    let parent = manifest.root.entries;
    const segments = input.path.split("/");
    for (let i = 0; i < segments.length; i++) {
      const existing = own(parent, segments[i]!);
      if (!existing) break;
      if (i < segments.length - 1) {
        if (existing.kind !== "directory")
          throw new WorkspaceTransferError(
            `A file blocks the destination folder: ${input.path}`,
            409,
          );
        parent = existing.entries;
      } else if (existing.kind !== input.kind) {
        throw new WorkspaceTransferError(
          `A ${existing.kind} already exists at ${input.path}. Choose a different name and upload again.`,
          409,
        );
      } else if (input.kind === "file") conflicts.push(input.path);
    }
  }
  return conflicts;
}

export function renameWorkspaceEntry<T extends TransferManifest>(
  source: T,
  path: string,
  name: string,
): T {
  if (
    !workspaceEntryRenameSchema.safeParse({ path, name, expectedRevision: 0 })
      .success
  )
    throw new WorkspaceTransferError("Enter a valid file or folder name", 400);
  const manifest = structuredClone(source);
  const entry = findEntry(manifest, path);
  const parts = path.split("/");
  const previousName = parts.pop()!;
  let parent = manifest.root.entries;
  for (const part of parts)
    parent = (own(parent, part) as TransferDirectory).entries;
  if (previousName === name) return manifest;
  if (own(parent, name))
    throw new WorkspaceTransferError(
      "A file or folder with this name already exists.",
      409,
    );
  assign(parent, name, entry);
  delete parent[previousName];
  return manifest;
}

export function removeWorkspaceEntry<T extends TransferManifest>(
  source: T,
  path: string,
): T {
  const manifest = structuredClone(source);
  const entry = findEntry(manifest, path);
  if (!path)
    throw new WorkspaceTransferError("Select a file or folder to delete", 400);
  const subtract = (entry: TransferEntry) => {
    if (entry.kind === "directory")
      Object.values(entry.entries).forEach(subtract);
    else {
      manifest.totals.files--;
      manifest.totals.bytes -= entry.size_bytes;
    }
  };
  const parts = path.split("/");
  const name = parts.pop()!;
  let parent = manifest.root.entries;
  for (const part of parts)
    parent = (own(parent, part) as TransferDirectory).entries;
  delete parent[name];
  subtract(entry);
  return manifest;
}

// Validate every collision before storing blobs; the caller publishes the copy
// with a single revision-guarded head update after all blob uploads succeed.
export function prepareWorkspaceUpload<T extends TransferManifest>(
  source: T,
  upload: WorkspaceUpload,
) {
  const manifest = structuredClone(source);
  const files: Array<{
    input: z.infer<typeof fileSchema>;
    entry: TransferFile;
  }> = [];
  for (const input of upload.entries) {
    const segments = input.path.split("/");
    const name = segments.pop()!;
    let entries = manifest.root.entries;
    for (const segment of segments) {
      let parent = own(entries, segment);
      if (!parent) {
        parent = { kind: "directory", entries: {} };
        assign(entries, segment, parent);
      }
      if (parent.kind !== "directory")
        throw new WorkspaceTransferError(
          `File blocks folder: ${input.path}`,
          409,
        );
      entries = parent.entries;
    }
    const existing = own(entries, name);
    if (input.kind === "directory") {
      if (existing?.kind === "file")
        throw new WorkspaceTransferError(
          `File blocks folder: ${input.path}`,
          409,
        );
      if (!existing) assign(entries, name, { kind: "directory", entries: {} });
    } else {
      if (existing?.kind === "directory")
        throw new WorkspaceTransferError(
          `Folder blocks file: ${input.path}`,
          409,
        );
      if (existing && !upload.replace)
        throw new WorkspaceTransferError(
          `File already exists: ${input.path}. Confirm replacement to upload it.`,
          409,
        );
      const entry: TransferFile = {
        kind: "file",
        blob_ref: "",
        size_bytes: base64Size(input.contentBase64),
        ...(input.mediaType ? { media_type: input.mediaType } : {}),
        executable: existing?.executable ?? false,
      };
      assign(entries, name, entry);
      files.push({ input, entry });
    }
  }
  const totals = { files: 0, bytes: 0 };
  const walk = (entries: Record<string, TransferEntry>) => {
    for (const entry of Object.values(entries)) {
      if (entry.kind === "directory") walk(entry.entries);
      else {
        totals.files++;
        totals.bytes += entry.size_bytes;
      }
    }
  };
  walk(manifest.root.entries);
  manifest.totals = totals;
  return { manifest, files };
}

function findEntry(manifest: TransferManifest, path: string): TransferEntry {
  if (!path) return { kind: "directory", entries: manifest.root.entries };
  if (!validWorkspaceTransferPath(path))
    throw new WorkspaceTransferError("Invalid relative path", 400);
  let entry: TransferEntry = {
    kind: "directory",
    entries: manifest.root.entries,
  };
  for (const part of path.split("/")) {
    const next: TransferEntry | undefined =
      entry.kind === "directory" ? own(entry.entries, part) : undefined;
    if (!next)
      throw new WorkspaceTransferError("File or folder not found", 404);
    entry = next;
  }
  return entry;
}

export function workspaceDownload(
  manifest: TransferManifest,
  path: string,
  workspaceName: string,
  readBlob: (ref: string) => Promise<Uint8Array<ArrayBuffer>>,
): Promise<Response> {
  const entry = findEntry(manifest, path);
  const name = path ? path.split("/").at(-1)! : workspaceName;
  const filename = entry.kind === "file" ? name : `${name}.zip`;
  const headers = {
    "content-type":
      entry.kind === "file" ? "application/octet-stream" : "application/zip",
    "content-disposition": `attachment; filename*=UTF-8''${encodeURIComponent(filename).replace(/['()*]/g, (c) => `%${c.charCodeAt(0).toString(16)}`)}`,
    "cache-control": "private, no-store",
    "x-content-type-options": "nosniff",
  };
  if (entry.kind === "file")
    return readBlob(entry.blob_ref).then(
      (bytes) => new Response(bytes, { headers }),
    );

  const entries: Array<{ path: string; entry: TransferEntry }> = [];
  const collect = (entry: TransferEntry, path: string) => {
    if (!validWorkspaceTransferPath(path))
      throw new WorkspaceTransferError(
        `Cannot archive unsafe path: ${path}`,
        400,
      );
    entries.push({
      path: entry.kind === "directory" ? `${path}/` : path,
      entry,
    });
    if (entry.kind === "directory")
      for (const [name, child] of Object.entries(entry.entries))
        collect(child, `${path}/${name}`);
  };
  collect(entry, name);

  async function* archive() {
    let chunks: Uint8Array<ArrayBuffer>[] = [];
    const zip = new Zip((error, data) => {
      if (error) throw error;
      chunks.push(new Uint8Array(data));
    });
    try {
      for (const { path, entry } of entries) {
        const file = new ZipDeflate(path, { level: 6 });
        file.os = 3;
        file.attrs =
          ((entry.kind === "directory"
            ? 0o40755
            : entry.executable
              ? 0o100755
              : 0o100644) <<
            16) >>>
          0;
        zip.add(file);
        file.push(
          entry.kind === "file"
            ? await readBlob(entry.blob_ref)
            : new Uint8Array(),
          true,
        );
        yield* chunks;
        chunks = [];
      }
      zip.end();
      yield* chunks;
    } finally {
      zip.terminate();
    }
  }
  const iterator = archive();
  return Promise.resolve(
    new Response(
      new ReadableStream({
        async pull(controller) {
          try {
            const next = await iterator.next();
            if (next.done) controller.close();
            else controller.enqueue(next.value);
          } catch (error) {
            controller.error(error);
          }
        },
        async cancel() {
          await iterator.return();
        },
      }),
      { headers },
    ),
  );
}
