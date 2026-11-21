import { DownloadIcon, InfoIcon } from "@/icons";
import { fileIcon } from "@/components/file-icon";
import { fileTypeName } from "@/components/workspace-file-meta";
import { Button } from "@/components/ui/button";
import { formatBytes } from "@/lib/format-bytes";
import type { FileModeProps, FileProvider } from "../types";

/// What there is to know about a file no provider shows, and its download.
function FileInfo({ file, download }: FileModeProps) {
  const Icon = fileIcon(file.name);
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 p-8 text-center">
      <Icon className="size-10 text-muted-foreground" aria-hidden />
      <div className="grid gap-1">
        <p className="text-sm font-medium">{file.name}</p>
        <p className="text-sm text-muted-foreground">
          {fileTypeName(file.name, file.mediaType)} · {formatBytes(file.sizeBytes)}
        </p>
        <p className="text-sm text-muted-foreground">There is no preview for this kind of file.</p>
      </div>
      {download && (
        <Button variant="secondary" size="sm" onClick={download}>
          <DownloadIcon data-icon="inline-start" />
          Download
        </Button>
      )}
    </div>
  );
}

/// Anything no other provider takes: the file's details and a download.
export const fallbackProvider: FileProvider = {
  id: "fallback",
  label: "File",
  match: () => 0,
  content: "none",
  modes: [{ id: "info", label: "Details", icon: InfoIcon, editable: false, component: FileInfo }],
};
