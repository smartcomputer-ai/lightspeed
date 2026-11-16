import { Suspense, type ReactNode } from "react";
import { DetailHeader, type HeaderTool, type MainHeaderTool } from "@/components/detail-header";
import { LoadingNote } from "@/components/page";
import { Button } from "@/components/ui/button";
import type { WorkspaceFileEditing } from "./editing";
import { effectiveMediaType } from "./media-types";
import { canEdit } from "./registry";
import type { FileContent, FileFacts, FileMode, FileProvider } from "./types";

/// The mode a provider shows a file in: the remembered one where the
/// provider has it, else its first.
export function modeOf(provider: FileProvider, remembered: string): FileMode {
  return provider.modes.find((mode) => mode.id === remembered) ?? provider.modes[0]!;
}

/// Shows one file through its provider: the header with the mode toggle
/// (only when the provider has more than one mode) and the main tools (Save
/// for a provider that edits, then the page's own), the editing session's
/// notices, and the mode's view. See docs/roadmap/u114-file-providers.md.
export function FileHost({
  header,
  facts,
  provider,
  bytes,
  loading,
  loadError,
  notice,
  unavailable,
  editing,
  writable,
  mode: remembered,
  onModeChange,
  download,
}: {
  header: {
    back?: { to: string; label: string };
    title: ReactNode;
    titleHint?: string;
    titleClassName?: string;
    tags?: ReactNode;
    /// The page's main tools, after Save.
    tools: MainHeaderTool[];
  };
  facts: FileFacts;
  /// Null while there is nothing to decide on yet (the content loads).
  provider: FileProvider | null;
  /// The loaded content, for providers that read bytes.
  bytes: Uint8Array | null;
  loading: boolean;
  loadError: Error | null;
  /// A notice of the page's own, above the view (e.g. the workspace could not be read).
  notice?: ReactNode;
  /// The file or its workspace can no longer be read (deleted, or the tree failed).
  unavailable: boolean;
  editing: WorkspaceFileEditing;
  /// The user may change the file.
  writable: boolean;
  /// The remembered mode id ("" for the provider's default).
  mode: string;
  onModeChange: (mode: string) => void;
  download?: () => void;
}) {
  const mode = provider ? modeOf(provider, remembered) : null;
  const edits = !!provider && canEdit(provider);
  const saves = writable && edits;
  const content: FileContent | null = !provider
    ? null
    : provider.content === "text"
      ? { kind: "text", text: editing.value }
      : provider.content === "bytes" && bytes
        ? { kind: "bytes", bytes, mediaType: effectiveMediaType(facts) ?? "application/octet-stream" }
        : provider.content === "none"
          ? { kind: "none" }
          : null;

  const subtools: HeaderTool[] = provider && provider.modes.length > 1
    ? [{
        kind: "choice",
        key: "mode",
        label: `${provider.label} view`,
        appearance: "toggle",
        value: mode!.id,
        options: provider.modes.map((option) => ({
          value: option.id,
          label: option.label,
          ...(option.hint ? { hint: option.hint } : {}),
          icon: <option.icon data-icon="inline-start" />,
        })),
        onChange: onModeChange,
      }]
    : [];
  const tools: MainHeaderTool[] = [
    ...(saves
      ? [{
          kind: "save" as const,
          key: "save",
          label: "Save",
          dirty: editing.dirty,
          saving: editing.saving,
          // A failed save, while its changes are still unsaved.
          error: editing.error !== null && editing.dirty,
          hint: "Save (⌘S / Ctrl+S)",
          keyShortcuts: "Meta+S Control+S",
          disabled: !editing.canSave,
          onSave: editing.save,
        }]
      : []),
    ...header.tools,
  ];

  const View = mode?.component;
  return (
    <div
      className="flex min-h-0 flex-1 flex-col"
      onKeyDown={(event) => {
        if (
          !saves ||
          event.nativeEvent.isComposing ||
          !(event.metaKey || event.ctrlKey) ||
          event.altKey ||
          event.shiftKey ||
          event.key.toLowerCase() !== "s"
        )
          return;
        event.preventDefault();
        if (!event.repeat) editing.save();
      }}
    >
      <DetailHeader {...header} subtools={subtools} tools={tools} />
      {editing.changedExternally && (
        <div role="status" className="flex items-center gap-3 border-b px-4 py-2 text-sm">
          <span>{unavailable ? "The file or workspace is no longer available." : "The workspace changed while you were editing."} Your draft is preserved.</span>
          <Button variant="outline" size="sm" onClick={editing.reload}>Discard draft and reload</Button>
        </div>
      )}
      {notice}
      {editing.error && <p className="border-b px-4 py-2 text-sm text-destructive">{editing.error}</p>}
      <div className="min-h-0 flex-1 overflow-y-auto">
        {loading && !content && (
          <div className="p-4">
            <LoadingNote />
          </div>
        )}
        {loadError && <p className="p-4 text-sm text-destructive">{loadError.message}</p>}
        {View && content && (
          <Suspense fallback={<div className="p-4"><LoadingNote /></div>}>
            <View
              file={facts}
              content={content}
              {...(mode!.editable
                ? { editing: { value: content, readOnly: !writable || editing.loading, change: (next: FileContent) => { if (next.kind === "text") editing.change(next.text); } } }
                : {})}
              {...(download ? { download } : {})}
            />
          </Suspense>
        )}
      </div>
    </div>
  );
}
