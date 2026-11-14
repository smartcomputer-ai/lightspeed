import { useState } from "react";
import { CopiedIcon, CopyIcon, ExecuteToolIcon, NewTabIcon } from "@/icons";
import type { VfsFileEntry } from "@/api";
import type { MetaItem } from "@/components/meta-toolbar";
import { fileIcon } from "@/components/file-icon";
import { formatBytes } from "@/lib/composer-attachments";

/// The meta toolbar of an open workspace file, beside its path: what kind
/// of file it is, how big (and, for text, how many lines), whether it runs,
/// then the content hash and the workspace revision it was
/// loaded at, whose details hold the full hash and the blob viewer link.
/// Every fact is about the saved file, not an unsaved draft.

/// Names for formats by extension, where the media type alone says too little.
const TYPE_NAMES: Record<string, string> = {
  ts: "TypeScript", mts: "TypeScript", cts: "TypeScript", tsx: "TypeScript JSX",
  js: "JavaScript", mjs: "JavaScript", cjs: "JavaScript", jsx: "JavaScript JSX",
  py: "Python", rs: "Rust", go: "Go", java: "Java", kt: "Kotlin", swift: "Swift",
  rb: "Ruby", php: "PHP", c: "C", h: "C header", cpp: "C++", cc: "C++", hpp: "C++ header",
  cs: "C#", sh: "Shell script", bash: "Shell script", zsh: "Shell script", ps1: "PowerShell",
  sql: "SQL", css: "CSS", scss: "SCSS", html: "HTML", htm: "HTML", vue: "Vue", svelte: "Svelte",
  md: "Markdown", markdown: "Markdown", mdx: "MDX", txt: "Plain text", log: "Log",
  json: "JSON", jsonl: "JSON Lines", yaml: "YAML", yml: "YAML", toml: "TOML", xml: "XML",
  ini: "INI", env: "Environment", csv: "CSV", tsv: "TSV",
  png: "PNG image", jpg: "JPEG image", jpeg: "JPEG image", gif: "GIF image", webp: "WebP image",
  svg: "SVG image", pdf: "PDF", zip: "ZIP archive", gz: "Gzip archive", tar: "Tar archive",
};

/// Files named for what they are, without an extension.
const NAMED_TYPES: Record<string, string> = {
  dockerfile: "Dockerfile", containerfile: "Containerfile", makefile: "Makefile",
  license: "Plain text", ".gitignore": "Git ignore", ".env": "Environment",
};

/// What kind of file a name and media type make, e.g. "TypeScript",
/// "PNG image"; the media type's subtype, then the extension, for others.
export function fileTypeName(name: string, mediaType: string | undefined): string {
  const lower = name.trim().toLowerCase();
  const named = NAMED_TYPES[lower];
  if (named) return named;
  const dot = lower.lastIndexOf(".");
  const extension = dot > 0 ? lower.slice(dot + 1) : "";
  const known = TYPE_NAMES[extension];
  if (known) return known;
  const mime = mediaType?.split(";")[0]?.trim().toLowerCase();
  if (mime) {
    const [kind, subtype] = mime.split("/");
    const short = subtype?.replace(/^(x-|vnd\.)/, "").split(/[+.]/)[0]?.toUpperCase();
    if (short && (kind === "image" || kind === "audio" || kind === "video")) return `${short} ${kind}`;
    if (mime === "text/plain") return "Plain text";
  }
  return extension ? `${extension.toUpperCase()} file` : "File";
}

/// The lines of a text: a final newline ends the last line, not a new one.
export function lineCount(text: string): number {
  if (text === "") return 0;
  return text.split("\n").length - (text.endsWith("\n") ? 1 : 0);
}

const plural = (count: number, word: string) => `${count.toLocaleString()} ${word}${count === 1 ? "" : "s"}`;

/// The hash's digest, without its algorithm: "sha256:abc…" → "abc…".
const digestOf = (blobRef: string) => blobRef.slice(blobRef.indexOf(":") + 1);

export function fileMetaItems({
  name,
  file,
  text,
  revision,
  blobViewerHref,
  unsaved = false,
}: {
  name: string;
  file: VfsFileEntry;
  /** The saved file's text, for a text file. */
  text: string | null;
  /** The workspace revision the file was loaded at. */
  revision: number | undefined;
  /** The saved file in the blob viewer. */
  blobViewerHref: string | null;
  /** The editor holds changes not saved yet, which the blob viewer will not show. */
  unsaved?: boolean;
}): MetaItem[] {
  const type = fileTypeName(name, file.media_type);
  const lines = text === null ? null : lineCount(text);
  const size = formatBytes(file.size_bytes);
  const sizeText = lines === null ? size : `${size} · ${plural(lines, "line")}`;
  const digest = digestOf(file.blob_ref);
  const items: MetaItem[] = [
    {
      key: "type",
      label: `Type: ${type}`,
      icon: fileIcon(name),
      text: type,
      tooltip: file.media_type ? `${type} · ${file.media_type}` : `${type} · no media type recorded`,
      priority: 9,
    },
    {
      key: "size",
      label: `Size: ${sizeText}`,
      value: sizeText,
      tooltip: [plural(file.size_bytes, "byte"), lines !== null && plural(lines, "line"), "as saved"].filter(Boolean).join(" · "),
      priority: 8,
    },
  ];
  if (file.executable) {
    items.push({
      key: "executable",
      label: "Executable",
      icon: ExecuteToolIcon,
      text: "Executable",
      tooltip: "Executable: runs as a program in an environment",
      priority: 6,
    });
  }
  items.push({
    key: "content",
    label: `Content hash ${file.blob_ref}`,
    value: <span className="font-mono">{digest.slice(0, 8)}</span>,
    text: <span className="font-mono">{digest.slice(0, 8)}</span>,
    tooltip: "Content hash: open for the full hash and the blob viewer",
    priority: 3,
    details: <FileContentDetails blobRef={file.blob_ref} revision={revision} blobViewerHref={blobViewerHref} unsaved={unsaved} />,
  });
  if (revision !== undefined) {
    items.push({
      key: "revision",
      label: `Workspace revision ${revision}`,
      value: `r${revision}`,
      text: `r${revision}`,
      tooltip: `Loaded at workspace revision ${revision}`,
      priority: 2,
    });
  }
  return items;
}

/// The content hash item's popover: the full hash to copy, the revision
/// it was loaded at, and the saved file in the blob viewer.
function FileContentDetails({ blobRef, revision, blobViewerHref, unsaved }: {
  blobRef: string;
  revision: number | undefined;
  blobViewerHref: string | null;
  unsaved: boolean;
}) {
  const [copied, setCopied] = useState(false);
  const copy = () => {
    void navigator.clipboard.writeText(blobRef).then(() => {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    });
  };
  const Copy = copied ? CopiedIcon : CopyIcon;
  return (
    <div className="grid gap-1">
      <div className="px-2 pt-1 pb-1.5">
        <p className="text-sm font-medium">Content</p>
        <p className="text-xs text-muted-foreground">
          Stored by its content: the same bytes always have this hash.
          {revision !== undefined && ` Loaded at workspace revision ${revision}.`}
        </p>
      </div>
      <div className="flex items-start gap-2 rounded-md px-2 py-1.5">
        <code className="min-w-0 flex-1 font-mono text-xs break-all">{blobRef}</code>
        <button
          type="button"
          onClick={copy}
          aria-label={copied ? "Copied" : "Copy content hash"}
          className="cursor-action shrink-0 rounded p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground"
        >
          <Copy className="size-3.5" />
        </button>
      </div>
      {blobViewerHref && (
        <a
          href={blobViewerHref}
          target="_blank"
          rel="noopener noreferrer"
          aria-label="Open in blob viewer (new tab)"
          title={unsaved ? "Open saved version in blob viewer (new tab)" : "Open in blob viewer (new tab)"}
          className="flex items-center gap-2 rounded-md px-2 py-1.5 text-sm hover:bg-muted"
        >
          <NewTabIcon className="size-4 text-muted-foreground" />
          {unsaved ? "Open saved version in blob viewer" : "Open in blob viewer"}
        </a>
      )}
    </div>
  );
}
