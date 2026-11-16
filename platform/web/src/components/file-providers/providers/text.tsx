import { SourceViewIcon } from "@/icons";
import type { FileFacts, FileMode, FileModeProps, FileProvider } from "../types";
import { effectiveMediaType } from "../media-types";

/// Media types that are text, whatever the extension says.
const TEXT_MIMES = /^(text\/|application\/(json|toml|yaml|x-yaml|xml|javascript))/;

/// Whether the content is text: by media type, or (with none that says so)
/// by having no NUL byte in its first KiB.
export function isText(file: FileFacts): boolean {
  const type = effectiveMediaType(file);
  if (type && TEXT_MIMES.test(type)) return true;
  if (type?.startsWith("image/") || type === "application/pdf") return false;
  return !file.head.subarray(0, 1024).includes(0);
}

/// The file's text, as written, in a plain editor: editable where the host
/// gives it editing and the user may write.
export function TextSource({ content, editing }: FileModeProps) {
  const value = editing?.value ?? content;
  const text = value.kind === "text" ? value.text : "";
  const readOnly = !editing || editing.readOnly;
  return (
    <textarea
      className="block h-full w-full resize-none bg-transparent p-4 font-mono text-sm outline-none"
      value={text}
      onChange={(event) => editing?.change({ kind: "text", text: event.target.value })}
      spellCheck={false}
      readOnly={readOnly}
      aria-label={editing && !editing.readOnly ? "File contents" : "File contents (read only)"}
    />
  );
}

export const SOURCE_MODE: FileMode = {
  id: "source",
  label: "Source",
  icon: SourceViewIcon,
  editable: true,
  component: TextSource,
};

/// Text and code: their source, editable. No preview: a rendered view of
/// code or plain text would be the same text.
export const textProvider: FileProvider = {
  id: "text",
  label: "Text",
  match: (file) => (isText(file) ? 40 : null),
  content: "text",
  modes: [SOURCE_MODE],
};
