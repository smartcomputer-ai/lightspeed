import {
  MAX_WORKSPACE_UPLOAD_BYTES,
  MAX_WORKSPACE_UPLOAD_ENTRIES,
  validWorkspaceTransferPath,
  type WorkspaceUpload,
} from "@lightspeed-ai/platform-shared";

export type UploadEntry =
  | { kind: "file"; path: string; file: File }
  | { kind: "directory"; path: string };

export function validateUploadEntries(entries: UploadEntry[]) {
  if (!entries.length) throw new Error("No files or folders selected.");
  if (entries.length > MAX_WORKSPACE_UPLOAD_ENTRIES)
    throw new Error("Select at most 10,000 files and folders per upload.");
  let bytes = 0;
  const paths = new Set<string>();
  for (const entry of entries) {
    if (!validWorkspaceTransferPath(entry.path))
      throw new Error(`Invalid path: ${entry.path}`);
    if (paths.has(entry.path)) throw new Error(`Duplicate path: ${entry.path}`);
    paths.add(entry.path);
    if (entry.kind === "file") bytes += entry.file.size;
  }
  if (bytes > MAX_WORKSPACE_UPLOAD_BYTES)
    throw new Error("Select up to 32 MiB per upload.");
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
): Promise<UploadEntry[]> {
  const entries: UploadEntry[] = [];
  const walk = async (directory: UploadDirectoryHandle, path: string) => {
    entries.push({ kind: "directory", path });
    for await (const child of directory.values()) {
      if (entries.length >= MAX_WORKSPACE_UPLOAD_ENTRIES)
        throw new Error("Select at most 10,000 files and folders per upload.");
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
  validateUploadEntries(entries);
  return entries;
}

// Capture entries synchronously: browsers clear the drag data store after
// the drop handler returns. Directory readers can return multiple batches.
export async function droppedEntries(
  transfer: DataTransfer,
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
    if (result.length >= MAX_WORKSPACE_UPLOAD_ENTRIES)
      throw new Error("Select at most 10,000 files and folders per upload.");
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
  validateUploadEntries(result);
  return result;
}

export async function uploadPayload(
  entries: UploadEntry[],
  destination: string,
  expectedRevision: number,
  replace: boolean,
  progress: (done: number) => void,
): Promise<WorkspaceUpload> {
  validateUploadEntries(entries);
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
