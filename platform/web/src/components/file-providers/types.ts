import type { ComponentType } from "react";
import type { IconComponent } from "@/icons";

/// What a provider can look at to decide whether it handles a file.
export interface FileFacts {
  /// The file's path in its workspace.
  path: string;
  /// The last path segment.
  name: string;
  /// The extension, lower case and without the dot ("" when there is none).
  extension: string;
  /// The media type the workspace recorded, if any.
  mediaType?: string;
  sizeBytes: number;
  /// The first bytes of the content, for signatures (a PDF's `%PDF-`, a NUL
  /// byte in binary data).
  head: Uint8Array;
}

/// The content a mode is shown with: what its provider asked the host for.
export type FileContent =
  | { kind: "text"; text: string }
  | { kind: "bytes"; bytes: Uint8Array; mediaType: string }
  | { kind: "none" };

/// An editable mode's hold on the file: the draft (or the saved content when
/// there is none) and how to change it. The host owns the draft and saving.
export interface FileEditing {
  value: FileContent;
  change(next: FileContent): void;
  /// The user may not change the file, or it is not loaded yet.
  readOnly: boolean;
}

export interface FileModeProps {
  file: FileFacts;
  content: FileContent;
  /// Present in editable modes.
  editing?: FileEditing;
  /// Downloads the file, where the host can.
  download?: () => void;
}

/// One way a provider shows a file. A provider with one mode shows no
/// toggle; with several, the host offers them as a toggle group.
export interface FileMode {
  id: "preview" | "source" | "edit" | "info" | (string & {});
  label: string;
  icon: IconComponent;
  /// The toggle button's tooltip, when the label needs more.
  hint?: string;
  /// Whether this mode changes the file; the host then gives it `editing`.
  editable: boolean;
  /// The mode's view. A heavy view (an editor library) is a `React.lazy`
  /// component; the host shows it inside Suspense.
  component: ComponentType<FileModeProps>;
}

/// One way to handle a kind of file: which files it takes, what it needs
/// loaded, and its modes. See docs/roadmap/u114-file-providers.md.
export interface FileProvider {
  id: string;
  /// Names the provider, e.g. in the toggle's accessible name ("Markdown view").
  label: string;
  /// How well it handles the file: the highest score wins; null: not at all.
  match(file: FileFacts): number | null;
  /// What the host loads before showing a mode.
  content: "text" | "bytes" | "none";
  /// In toggle order; the first is the default.
  modes: readonly FileMode[];
}
