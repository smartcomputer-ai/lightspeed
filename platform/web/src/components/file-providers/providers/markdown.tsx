import { PreviewIcon } from "@/icons";
import { MarkdownContent } from "@/components/session/markdown-content";
import type { FileFacts, FileModeProps, FileProvider } from "../types";
import { effectiveMediaType } from "../media-types";
import { SOURCE_MODE, isText } from "./text";

/// The Markdown rendered: the draft where there is one, so a change shows
/// before it is saved.
function MarkdownPreview({ content, editing }: FileModeProps) {
  const value = editing?.value ?? content;
  return <MarkdownContent className="mx-auto max-w-5xl p-4">{value.kind === "text" ? value.text : ""}</MarkdownContent>;
}

const MARKDOWN_TYPES = new Set(["text/markdown", "text/x-markdown"]);
const MARKDOWN_EXTENSIONS = new Set(["md", "markdown"]);

/// Markdown by its media type, or by its extension where the recorded type
/// only says text.
function isMarkdown(file: FileFacts): boolean {
  const type = effectiveMediaType(file) ?? "";
  return MARKDOWN_TYPES.has(type) || (MARKDOWN_EXTENSIONS.has(file.extension) && type.startsWith("text/"));
}

/// Markdown: rendered (the default) or its source, editable.
export const markdownProvider: FileProvider = {
  id: "markdown",
  label: "Markdown",
  match: (file) => (isMarkdown(file) && isText(file) ? 80 : null),
  content: "text",
  modes: [
    {
      id: "preview",
      label: "Preview",
      icon: PreviewIcon,
      hint: "Preview Markdown",
      // Shows the draft, but is not where it is changed.
      editable: false,
      component: MarkdownPreview,
    },
    SOURCE_MODE,
  ],
};
