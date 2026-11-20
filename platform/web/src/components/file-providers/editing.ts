import { useEffect, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api, type VfsFileEntry, type WorkspaceRow, type WorkspaceTree } from "@/api";
import { mediaTypeFor } from "./media-types";

/// A move of the open file inside its workspace, done here: the revision it
/// was made at and the one it made.
export interface FileRelocation {
  identity: number;
  before: number;
  after: number;
}

/// The editing session of one workspace text file: the draft, the workspace
/// revision it is based on, and saving. A save carries that revision and is
/// refused (409) when the workspace moved on; the draft is kept either way.
/// Providers change the draft through `change`; the host shows the state.
export function useWorkspaceFileEditing({
  universeId,
  workspaceId,
  filePath,
  fileIdentity,
  relocation,
  tree,
  treeError,
  file,
  text,
  placeholder,
}: {
  universeId: string;
  workspaceId: string;
  filePath: string;
  /// Changes when another file opens, which starts a new session.
  fileIdentity: number;
  relocation: FileRelocation | null;
  tree: WorkspaceTree | undefined;
  treeError: Error | null;
  file: VfsFileEntry | null;
  /// The saved text, when the file is text and loaded.
  text: string | null;
  /// The loaded text is still the previous version's, while the new one loads.
  placeholder: boolean;
}) {
  const queryClient = useQueryClient();
  const [editor, setEditor] = useState<{
    identity: number;
    draft: string | null;
    saved: { text: string; previousBlob: string } | null;
    base: { text: string; revision: number } | null;
    error: string | null;
  }>({ identity: fileIdentity, draft: null, saved: null, base: null, error: null });
  if (editor.identity !== fileIdentity) {
    setEditor({ identity: fileIdentity, draft: null, saved: null, base: null, error: null });
  }
  // Only a successful local relocation may advance an existing draft's base.
  // Its compare-and-set proves no intervening workspace write was skipped.
  useEffect(() => {
    if (relocation?.identity !== fileIdentity) return;
    setEditor((current) => current.base?.revision === relocation.before
      ? { ...current, base: { ...current.base, revision: relocation.after } } : current);
  }, [relocation, fileIdentity]);

  // A successful save becomes the baseline immediately. Keep its draft until
  // the refreshed blob is ready, and preserve anything typed during the save.
  useEffect(() => {
    if (placeholder || text === null || !file) return;
    setEditor((current) => {
      if (
        current.identity !== fileIdentity ||
        !current.saved ||
        (file.blob_ref === current.saved.previousBlob && text !== current.saved.text)
      )
        return current;
      return {
        ...current,
        draft: current.draft === current.saved.text ? null : current.draft,
        base: current.draft === current.saved.text ? null : current.base,
        saved: null,
      };
    });
  }, [fileIdentity, file?.blob_ref, text, placeholder, editor.saved, file]);

  const invalidate = () =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: ["workspace-tree", universeId, workspaceId] }),
      queryClient.invalidateQueries({ queryKey: ["workspaces", universeId] }),
    ]);

  const save = useMutation({
    mutationFn: (submission: {
      contentText: string;
      path: string;
      revision: number;
      identity: number;
      previousBlob: string;
    }) =>
      api<{ workspace: WorkspaceRow }>(
        "PUT",
        `/api/ui/universes/${universeId}/workspaces/${workspaceId}/files/${submission.path.split("/").map(encodeURIComponent).join("/")}`,
        {
          contentText: submission.contentText,
          expectedRevision: submission.revision,
          ...(mediaTypeFor(submission.path) ? { mediaType: mediaTypeFor(submission.path) } : {}),
        },
      ),
    onSuccess: async (result, submission) => {
      setEditor((current) =>
        current.identity === submission.identity
          ? {
              ...current,
              error: null,
              base: { text: submission.contentText, revision: result.workspace.revision },
              saved: { text: submission.contentText, previousBlob: submission.previousBlob },
            }
          : current,
      );
      await invalidate();
    },
    onError: (err, submission) =>
      setEditor((current) => (current.identity === submission.identity ? { ...current, error: err.message } : current)),
  });

  const value = editor.draft ?? editor.saved?.text ?? text ?? "";
  const dirty = editor.draft !== null && editor.draft !== (editor.saved?.text ?? editor.base?.text ?? text);
  const changedExternally = dirty && editor.base !== null &&
    (treeError !== null || !file || tree?.workspace.revision !== editor.base.revision);
  const canSave = dirty && !save.isPending && !changedExternally && !!file && !placeholder && !treeError;

  return {
    /// The draft, or the saved text.
    value,
    /// Whether there is a draft at all (even one equal to the saved text).
    hasDraft: editor.draft !== null,
    dirty,
    saving: save.isPending,
    /// The last save's error, while it is not resolved.
    error: editor.error,
    /// The workspace (or the file) changed under a draft, which can then not be saved.
    changedExternally,
    canSave,
    /// The text cannot be changed yet: the previous version is still shown.
    loading: placeholder && editor.draft === null,
    change(next: string) {
      setEditor((current) => {
        const base = current.base ?? { text: text ?? "", revision: tree!.workspace.revision };
        return next === base.text && !current.saved
          ? { ...current, draft: null, base: null, error: null }
          : { ...current, draft: next, base };
      });
    },
    save() {
      if (!canSave) return;
      save.mutate({
        contentText: value,
        path: filePath,
        revision: editor.base?.revision ?? tree!.workspace.revision,
        identity: fileIdentity,
        previousBlob: file!.blob_ref,
      });
    },
    /// Drops the draft and loads the workspace again.
    reload() {
      setEditor({ identity: fileIdentity, draft: null, saved: null, base: null, error: null });
      void invalidate();
    },
  };
}

export type WorkspaceFileEditing = ReturnType<typeof useWorkspaceFileEditing>;
