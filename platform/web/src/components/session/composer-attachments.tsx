import { FileText, LoaderCircle, RotateCw, TriangleAlert, X } from "lucide-react";
import { attachmentLabel, formatBytes, type ComposerAttachment } from "@/lib/composer-attachments";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

/// Attached files above the message field. Each chip owns its state:
/// uploading shows a spinner over the thumbnail, a failed upload offers a
/// retry, and every chip can be removed.
export function AttachmentStrip({ items, disabled, onRemove, onRetry }: {
  items: ComposerAttachment[];
  disabled?: boolean;
  onRemove: (id: string) => void;
  onRetry: (id: string) => void;
}) {
  if (!items.length) return null;
  return (
    <ul aria-label="Attachments" className="flex flex-wrap gap-2 px-2.5 pt-2.5">
      {items.map((item) => <AttachmentChip key={item.id} item={item} disabled={disabled} onRemove={onRemove} onRetry={onRetry} />)}
    </ul>
  );
}

function AttachmentChip({ item, disabled, onRemove, onRetry }: {
  item: ComposerAttachment;
  disabled?: boolean;
  onRemove: (id: string) => void;
  onRetry: (id: string) => void;
}) {
  const failed = item.status === "failed";
  const uploading = item.status === "uploading";
  const detail = failed
    ? "Upload failed"
    : uploading
      ? "Uploading…"
      : `${attachmentLabel(item)} · ${formatBytes(item.size)}`;
  return (
    <li
      className={cn(
        "group/chip flex h-11 max-w-60 min-w-0 items-center gap-2 rounded-lg border bg-muted/40 py-1 pr-1 pl-1 animate-in fade-in zoom-in-95 duration-150",
        failed && "border-destructive/40 bg-destructive/5",
      )}
      title={failed ? item.error : item.name}
    >
      <span className="relative flex size-9 shrink-0 items-center justify-center overflow-hidden rounded-md bg-muted text-muted-foreground">
        {item.previewUrl
          ? <img src={item.previewUrl} alt="" className="size-full object-cover" />
          : failed ? <TriangleAlert className="size-4 text-destructive" /> : <FileText className="size-4" />}
        {uploading && (
          <span className="absolute inset-0 flex items-center justify-center bg-background/70">
            <LoaderCircle className="size-4 animate-spin" aria-hidden />
          </span>
        )}
      </span>
      <span className="flex min-w-0 flex-col leading-tight">
        <span className="truncate text-xs font-medium">{item.name}</span>
        <span className={cn("truncate text-[11px] text-muted-foreground", failed && "text-destructive")} role={failed ? "alert" : undefined}>
          {detail}
        </span>
      </span>
      <span className="flex shrink-0 items-center">
        {failed && (
          <Button type="button" variant="ghost" size="icon-xs" aria-label={`Retry uploading ${item.name}`} title="Retry upload"
            disabled={disabled} onClick={() => onRetry(item.id)}>
            <RotateCw />
          </Button>
        )}
        <Button type="button" variant="ghost" size="icon-xs" aria-label={`Remove ${item.name}`} title="Remove"
          disabled={disabled} onClick={() => onRemove(item.id)}>
          <X />
        </Button>
      </span>
    </li>
  );
}
