import {
  FileArchiveIcon,
  FileAudioIcon,
  FileCIcon,
  FileCodeIcon,
  FileCppIcon,
  FileCSharpIcon,
  FileCssIcon,
  FileCsvIcon,
  FileDocIcon,
  FileHtmlIcon,
  FileIcon,
  FileImageIcon,
  FileIniIcon,
  FileJpgIcon,
  FileJsIcon,
  FileJsxIcon,
  FileMdIcon,
  FilePdfIcon,
  FilePngIcon,
  FilePptIcon,
  FilePyIcon,
  FileRsIcon,
  FileSqlIcon,
  FileSvgIcon,
  FileTsIcon,
  FileTsxIcon,
  FileTxtIcon,
  FileVideoIcon,
  FileVueIcon,
  FileXlsIcon,
  FileZipIcon,
  type IconComponent,
} from "@/icons";

/// The icon for a file, from its name: its extension (or, for a few files
/// known without one, its whole name). A format with an icon of its own
/// takes it; others fall back to their kind (code, image, audio, video,
/// archive); anything unknown is a plain file.

const BY_EXTENSION: Record<string, IconComponent> = {};
function add(icon: IconComponent, extensions: string) {
  for (const extension of extensions.split(" ")) BY_EXTENSION[extension] = icon;
}

// Formats with an icon of their own, and their close relatives.
add(FilePdfIcon, "pdf");
add(FileDocIcon, "doc docx odt rtf pages");
add(FileXlsIcon, "xls xlsx xlsm ods numbers");
add(FilePptIcon, "ppt pptx odp key");
add(FileTxtIcon, "txt text log rst adoc org");
add(FileMdIcon, "md markdown mdx");
add(FileCsvIcon, "csv tsv");
add(FilePngIcon, "png");
add(FileJpgIcon, "jpg jpeg");
add(FileSvgIcon, "svg");
add(FileTsIcon, "ts mts cts");
add(FileTsxIcon, "tsx");
add(FileJsIcon, "js mjs cjs");
add(FileJsxIcon, "jsx");
add(FilePyIcon, "py pyi pyw ipynb");
add(FileRsIcon, "rs");
add(FileCIcon, "c h");
add(FileCppIcon, "cpp cc cxx hpp hh hxx");
add(FileCSharpIcon, "cs csx");
add(FileCssIcon, "css scss sass less");
add(FileHtmlIcon, "html htm xhtml");
add(FileVueIcon, "vue");
add(FileSqlIcon, "sql");
add(FileIniIcon, "ini cfg conf config env properties editorconfig");
add(FileZipIcon, "zip");

// Kinds, for formats without an icon of their own.
add(
  FileCodeIcon,
  "json jsonc json5 jsonl yaml yml toml xml lock " +
    "go java kt kts scala groovy gradle swift m mm rb php pl lua dart r jl " +
    "ex exs erl hs clj cljs elm fs fsx ml zig nim sol " +
    "sh bash zsh fish ps1 psm1 bat cmd " +
    "svelte astro graphql gql proto tf tfvars hcl nix cmake mk wat",
);
add(FileImageIcon, "gif webp bmp ico tif tiff avif heic heif psd raw");
add(FileAudioIcon, "mp3 wav ogg oga flac m4a aac opus aiff wma mid midi");
add(FileVideoIcon, "mp4 m4v mov webm mkv avi wmv flv mpg mpeg 3gp");
add(FileArchiveIcon, "tar gz tgz bz2 xz 7z rar zst lz4 jar war iso dmg");

/// Files known by their whole name, without an extension that says what they are.
const BY_NAME: Record<string, IconComponent> = {
  dockerfile: FileCodeIcon,
  containerfile: FileCodeIcon,
  makefile: FileCodeIcon,
  justfile: FileCodeIcon,
  jenkinsfile: FileCodeIcon,
  procfile: FileCodeIcon,
  gemfile: FileCodeIcon,
  rakefile: FileCodeIcon,
  license: FileTxtIcon,
  ".gitignore": FileIniIcon,
  ".gitattributes": FileIniIcon,
  ".dockerignore": FileIniIcon,
  ".npmrc": FileIniIcon,
  ".nvmrc": FileIniIcon,
  ".prettierrc": FileIniIcon,
  ".env": FileIniIcon,
};

/// The icon for a file name (with its extension), e.g. "report.pdf" or
/// "Dockerfile".
export function fileIcon(name: string): IconComponent {
  const lower = name.trim().toLowerCase();
  const known = BY_NAME[lower];
  if (known) return known;
  // ".env.local" and the like are settings too.
  if (lower.startsWith(".env.")) return FileIniIcon;
  const dot = lower.lastIndexOf(".");
  // A leading dot alone names a dotfile, not an extension.
  if (dot <= 0) return lower.startsWith(".") ? FileIniIcon : FileIcon;
  return BY_EXTENSION[lower.slice(dot + 1)] ?? FileIcon;
}
