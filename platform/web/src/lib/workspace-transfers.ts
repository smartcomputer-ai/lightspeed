import {
  DEFAULT_WORKSPACE_UPLOAD_LIMITS,
  formatUploadBytes,
  validWorkspaceTransferPath,
  type WorkspaceUpload,
  type WorkspaceUploadLimits,
} from "@lightspeed-ai/platform-shared";

function tooManyEntries(limits: WorkspaceUploadLimits): Error {
  return new Error(
    `Select at most ${limits.maxEntries.toLocaleString("en-US")} files and folders per upload.`,
  );
}

export type UploadEntry =
  | { kind: "file"; path: string; file: File }
  | { kind: "directory"; path: string };

/// Checks a selection against the deployment's limits, which the workspace
/// tree reports; the defaults apply until it has loaded.
export function validateUploadEntries(
  entries: UploadEntry[],
  limits: WorkspaceUploadLimits = DEFAULT_WORKSPACE_UPLOAD_LIMITS,
) {
  if (!entries.length) throw new Error("No files or folders selected.");
  if (entries.length > limits.maxEntries) throw tooManyEntries(limits);
  let bytes = 0;
  const paths = new Set<string>();
  for (const entry of entries) {
    if (!validWorkspaceTransferPath(entry.path))
      throw new Error(`Invalid path: ${entry.path}`);
    if (paths.has(entry.path)) throw new Error(`Duplicate path: ${entry.path}`);
    paths.add(entry.path);
    if (entry.kind === "file") bytes += entry.file.size;
  }
  if (bytes > limits.maxBytes)
    throw new Error(`Select up to ${formatUploadBytes(limits.maxBytes)} per upload.`);
}

export function selectedFiles(files: FileList | File[]): UploadEntry[] {
  return Array.from(files).map((file) => ({
    kind: "file",
    path: file.webkitRelativePath || file.name,
    file,
  }));
}

export interface UploadDirectoryHandle {
  kind: "directory";
  name: string;
  values(): AsyncIterable<
    | UploadDirectoryHandle
    | { kind: "file"; name: string; getFile(): Promise<File> }
  >;
}

export async function directoryEntries(
  root: UploadDirectoryHandle,
  limits: WorkspaceUploadLimits = DEFAULT_WORKSPACE_UPLOAD_LIMITS,
): Promise<UploadEntry[]> {
  const entries: UploadEntry[] = [];
  const walk = async (directory: UploadDirectoryHandle, path: string) => {
    entries.push({ kind: "directory", path });
    for await (const child of directory.values()) {
      if (entries.length >= limits.maxEntries) throw tooManyEntries(limits);
      const childPath = `${path}/${child.name}`;
      if (child.kind === "directory") await walk(child, childPath);
      else
        entries.push({
          kind: "file",
          path: childPath,
          file: await child.getFile(),
        });
    }
  };
  await walk(root, root.name);
  validateUploadEntries(entries, limits);
  return entries;
}

// Capture entries synchronously: browsers clear the drag data store after
// the drop handler returns. Directory readers can return multiple batches.
export async function droppedEntries(
  transfer: DataTransfer,
  limits: WorkspaceUploadLimits = DEFAULT_WORKSPACE_UPLOAD_LIMITS,
): Promise<UploadEntry[]> {
  const items = Array.from(transfer.items ?? []).filter(
    (item) => item.kind === "file",
  );
  const roots = items.map((item) => ({
    entry: item.webkitGetAsEntry?.(),
    file: item.getAsFile(),
  }));
  const result: UploadEntry[] = [];
  const visit = async (entry: FileSystemEntry, parent: string) => {
    const path = parent ? `${parent}/${entry.name}` : entry.name;
    if (result.length >= limits.maxEntries) throw tooManyEntries(limits);
    if (entry.isFile) {
      const file = await new Promise<File>((resolve, reject) =>
        (entry as FileSystemFileEntry).file(resolve, reject),
      );
      result.push({ kind: "file", path, file });
    } else if (entry.isDirectory) {
      result.push({ kind: "directory", path });
      const reader = (entry as FileSystemDirectoryEntry).createReader();
      for (;;) {
        const batch = await new Promise<FileSystemEntry[]>((resolve, reject) =>
          reader.readEntries(resolve, reject),
        );
        if (!batch.length) break;
        for (const child of batch) await visit(child, path);
      }
    }
  };
  if (!roots.length) result.push(...selectedFiles(transfer.files));
  for (const { entry, file } of roots) {
    if (entry) await visit(entry, "");
    else if (file) result.push({ kind: "file", path: file.name, file });
    else
      throw new Error(
        "Your browser could not read this item. Use Upload files or Upload folder.",
      );
  }
  validateUploadEntries(result, limits);
  return result;
}

export async function uploadPayload(
  entries: UploadEntry[],
  destination: string,
  expectedRevision: number,
  replace: boolean,
  progress: (done: number) => void,
  limits: WorkspaceUploadLimits = DEFAULT_WORKSPACE_UPLOAD_LIMITS,
): Promise<WorkspaceUpload> {
  validateUploadEntries(entries, limits);
  const prefix = destination.trim().replace(/^\/+|\/+$/g, "");
  if (prefix && !validWorkspaceTransferPath(prefix))
    throw new Error("Enter a valid destination folder.");
  const result: WorkspaceUpload["entries"] = [];
  for (const entry of entries) {
    const path = prefix ? `${prefix}/${entry.path}` : entry.path;
    if (entry.kind === "directory") result.push({ kind: "directory", path });
    else {
      const contentBase64 = await new Promise<string>((resolve, reject) => {
        const reader = new FileReader();
        reader.onerror = () =>
          reject(new Error(`Could not read ${entry.path}`));
        reader.onload = () => resolve(String(reader.result).split(",")[1]!);
        reader.readAsDataURL(entry.file);
      });
      result.push({
        kind: "file",
        path,
        contentBase64,
        mediaType: entry.file.type || undefined,
      });
    }
    progress(result.length);
  }
  return { entries: result, expectedRevision, replace };
}

export function workspaceBaseUrl(universeId: string, workspaceId: string) {
  return `/api/v1/universes/${encodeURIComponent(universeId)}/workspaces/${encodeURIComponent(workspaceId)}`;
}
