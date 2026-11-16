# U114: File Providers

Status: [in progress]

The workspace file page decides how to show a file in one component, with one classifier and a branch per kind. This plan replaces that with **file providers**: each provider handles some file types and offers one or more **modes** (preview, source, edit). A shared **file host** picks the provider, shows the mode toggle when there is more than one mode, loads the content the provider needs, and owns saving for every provider that can edit.

The aim is to add richer viewers and editors later without touching the page: the Monaco editor for code, HTML and JavaScript ([U109](u109-file-editor.md)), Office document previews, and image editing. Each of those becomes one provider.

## Today

The file page is `FileDetail` in `src/pages/WorkspacesPage.tsx`.

- **Classifying.** `classify()` sorts a file into `text`, `image`, `pdf` or `binary` from its media type, its extension (`TEXT_EXTENSIONS`, `IMAGE_EXTENSIONS`) and its first bytes (a PDF signature, or a NUL byte in the first KiB for binary). The blob viewer (`src/pages/BlobPage.tsx`) has a second classifier, `blobView()` in `src/lib/blob-view.ts`, with its own rules.
- **Showing.** Each kind is a branch in the page's JSX: a `<textarea>` for text, an `<img>` with a data URL for images, `PdfPreview` for PDFs, and a "no preview" line for binary files.
- **Modes.** Every text file gets the Source/Preview toggle, and Preview always renders Markdown, even for code or plain text. Whether Preview is on is page state (`preview` in `WorkspacesPage`), shared by every file and lost on reload.
- **Loading.** The whole file is fetched as base64 inside JSON (`GET /blobs/:blobRef`) and decoded in the browser, whatever its size.
- **Editing and saving.** Only text can be edited. The page keeps the editor state itself: the draft, the base text and workspace revision it was loaded at, the last save, and the last error. Saving is `PUT /workspaces/:id/files/:path` with `contentText` and `expectedRevision`. A stale revision returns 409. The page shows "The workspace changed while you were editing", keeps the draft, and offers only "Discard draft and reload". ⌘S / Ctrl+S saves. The header shows the save state with the save button (Save, Saving…, Saved, Save failed).
- **Header.** The header tools from the shared header interface: the mode toggle as a subtool, and Save, Download and the file's ⋯ menu as the main tools.
- **Writing binary content.** The write API already takes `contentBase64` as well as `contentText`, but nothing in the UI uses it. Uploads go through the workspace's upload endpoint (`src/components/workspace-transfers.tsx`), with base64 JSON bodies, size limits, a conflict review and a progress line.

Problems this plan fixes:

- Adding a viewer means editing the page's classifier and its JSX, and keeping the blob viewer's classifier in step.
- Code and plain-text files offer a Markdown preview that is not meaningful for them.
- Editing and saving are written for text in a textarea. An editor with its own document model (Monaco), or an editor of binary content, has nowhere to plug in.
- Large or binary files are always loaded as base64 JSON, which Office and PDF viewers do not need and which costs memory.

## Common practice

- VS Code chooses a **custom editor** per file pattern, with a priority, and falls back to the text editor. A file can have several editors, and the user picks one with "Reopen With…". The editor declares what it can do (read-only or editable), and the workbench owns saving, dirty state and "the file changed on disk".
- GitHub and GitLab show a file with Preview and Code tabs only when there is something to preview (Markdown, images, notebooks, GeoJSON). Other files show the code view alone.
- Google Drive and SharePoint preview Office documents in the browser. Self-hosted products either convert documents on the server (often to PDF) or use client-side libraries. Microsoft's public Office viewer needs the file at a public URL, so it is not usable for private files.

## Design

### Providers and modes

A provider is a description plus lazily loaded components. It is chosen by matching the file; it never decides how the page looks around it.

```ts
/// One way to handle a kind of file.
interface FileProvider {
  id: string;                       // "markdown", "image", "monaco", …
  label: string;                    // shown where a choice is offered, e.g. "Open with…"
  /// How well it handles this file: higher wins, null means not at all.
  match(file: FileFacts): number | null;
  modes: FileMode[];                // in toggle order; the first is the default
  /// What the host loads before showing a mode.
  content: "text" | "bytes" | "url";
  /// Above this size the host offers a download instead.
  maxBytes?: number;
}

/// What a match can look at.
interface FileFacts {
  path: string;
  name: string;
  extension: string;
  mediaType?: string;               // as recorded in the workspace
  sizeBytes: number;
  head: Uint8Array;                 // the first bytes, for signatures
}

interface FileMode {
  id: "preview" | "source" | "edit" | string;
  label: string;                    // "Preview", "Source"
  icon: IconComponent;
  /// Whether this mode changes the file. The host gives it a draft and saving.
  editable: boolean;
  component: () => Promise<{ default: ComponentType<FileModeProps> }>;
}
```

- **One mode:** no toggle. Images and PDFs only have Preview.
- **Several modes:** the host shows them as the header's toggle subtool, as Source/Preview is today, with each mode's label and icon.
- **Matching:** every provider is asked, and the highest score wins. A fallback provider always matches with the lowest score. It shows the file's facts and a download button.
- **Choosing another provider:** where more than one matches, the ⋯ menu offers "Open with…". The default stays the best match.
- **Remembering the mode:** the last mode is kept per file in the settings store ([U113](u113-synced-settings.md)), as a device setting: one object per workspace, with a field per path (`workspaceFileMode`). A file opens in the mode it was last shown in; a file never switched opens in its provider's first mode.

Mode meanings:

| Mode | What it shows | Editable |
| --- | --- | --- |
| Preview | The file rendered as it will look: Markdown, an image, a PDF, a document | No |
| Source | The file's text, as written | Yes, where the provider and the user's role allow |
| Edit | Visual editing of the rendered file (an image crop, a document editor) | Yes |

A provider may add its own modes (for example "Diff" later), with the same contract.

### The file host

The host replaces the body of `FileDetail`. It is the only part that knows about workspaces, revisions and the API:

1. Picks the provider and mode.
2. Loads the content the provider asks for (`text`, `bytes` or `url`), within `maxBytes`.
3. Renders the mode's component with `FileModeProps`.
4. Owns the editing session and saving for editable modes.
5. Fills the header: the mode toggle as a subtool; Save (for editable modes), Download and the ⋯ menu as the main tools.

```ts
interface FileModeProps {
  file: FileFacts;
  content: FileContent;             // what the provider asked for
  /// Present in editable modes the user may change.
  editing?: FileEditing;
}

type FileContent =
  | { kind: "text"; text: string }
  | { kind: "bytes"; bytes: Uint8Array }
  | { kind: "url"; url: string };   // an object URL or a raw endpoint

interface FileEditing {
  /// The draft, or the saved content when there is none.
  value: FileContent;
  /// Report an edit. Simple editors pass the new content.
  change(next: FileContent): void;
  /// Editors with their own model (Monaco) register how to read it at save
  /// time and report only that something changed.
  setSerializer(serialize: () => Promise<FileContent>): void;
  markDirty(): void;
  readOnly: boolean;
}
```

Switching modes keeps the draft. A preview of a file with unsaved changes renders the draft, and says so.

### Saving

Saving moves out of the page into one shared editing session, used by every editable provider:

- **States.** Clean → Dirty → Saving (with progress and a phase: uploading, committing) → Saved, or → Failed. The save button shows these, and the header's ⋯ menu and ⌘S / Ctrl+S use the same session.
- **Content.** Text saves with `contentText`, as today. Bytes save with `contentBase64` up to a limit, and above it through an upload that reports progress (the upload endpoint, or a raw-bytes `PUT`, see open questions).
- **Progress.** A save reports what it is doing. Small saves show Saving…. Large ones show how far the upload is, on the save button and as the header's activity, the way uploads report today.
- **Conflicts.** Every save carries the revision it was based on, and a stale one is refused (409), as today. The host then offers three choices instead of one:
  - discard the draft and reload;
  - save over the newer version;
  - keep the draft as a copy beside the file.
- **Leaving with unsaved changes.** Navigating away or closing the tab while a draft is dirty asks first.
- **Errors.** A failed save keeps the draft and shows Save failed with the reason: conflict, too large, no permission, or network.

Autosave is not part of this plan. The session is shaped so a provider could ask for it later.

### Loading

- **Text:** decoded once by the host, within the provider's `maxBytes` (for example 5 MB). Above that the host offers a download.
- **Bytes:** for providers that parse the file themselves (Office, images to edit).
- **URL:** for viewers that read a URL, such as PDF and later Office viewers. This needs an endpoint that returns the raw bytes with the file's content type, instead of base64 JSON. Until it exists, the host makes an object URL from the fetched bytes.

### Safety

The rule in `src/lib/blob-view.ts` stays: stored HTML and SVG are never rendered as part of the app. A provider that renders HTML does it in a sandboxed `iframe` (`srcdoc`, no same origin, scripts off unless a later plan decides otherwise). No provider sends file contents to a third-party service.

### Where it lives

`src/components/file-providers/`:

- `types.ts` — the interfaces above.
- `registry.ts` — the providers, in one list, and `providerFor(file)`.
- `host.tsx` — the file host and its header tools.
- `editing.ts` — the editing session and saving.
- One folder per provider, with its mode components loaded lazily.

### The blob viewer

The blob viewer (`src/pages/BlobPage.tsx`, at `/u/:handle/blobs/:digest`) still needs to be refactored onto this system. It shows one stored blob, addressed by its digest: a workspace file's content, stored session content (long tool output, instructions), or media the model was shown. Workspace files, session transcripts and the session page link to it in a new tab, with a name, a type, and the session or workspace path as display hints in the URL.

It has its own classifier, `blobView()` in `src/lib/blob-view.ts`:

- Image signatures (PNG, JPEG, GIF, WebP) and the PDF signature decide first, whatever the link claims.
- Valid UTF-8 is text, labelled with the link's type. Anything else is binary.
- It has no extension tables and no recorded media type.

It shows images and PDFs, shows text with a Markdown preview toggle for every text blob, and offers anything else as a download.

Two classifiers disagree at the edges, and every new preview would have to be built twice:

- An SVG is an image in the workspace (by its type) and text in the blob viewer.
- Text that is not valid UTF-8 (Latin-1) is text in the workspace and binary in the blob viewer.
- The workspace offers the toggle only for Markdown; the blob viewer offers it for every text blob.

One system can serve both: the same registry and providers, and the same host in a read-only form for the blob viewer, which never offers editing or saving.

The blob viewer finds its provider from the file name in its link. Most links carry one: workspace files pass the file's name, and session media pass the attachment's name. One shared converter maps a file name to its media type (`mediaTypeFor()` in `src/components/file-providers/media-types.ts`, which the workspace already uses). The name becomes the facts' `name` and `extension`, its media type the facts' `mediaType`, and `providerFor()` picks the provider, as for a workspace file. A link's `type` is used only where the name has no known extension, such as stored transcript content, which is named by its label and passes `text/markdown`. A bare digest has neither, and the bytes decide alone.

The name comes from a URL anyone can change, so it chooses the provider but does not override the bytes where that matters. An image or a PDF is shown as one only when the bytes carry its signature; otherwise the file falls back to text or a download. HTML and SVG are never rendered as part of the app, whatever the name says (see [Safety](#safety)).

How the two views share the host, and what the blob viewer's header shows (the digest and the link's source instead of a workspace's revision), is decided in phase 3.

## Providers

### First set (today's behaviour, moved)

| Provider | Files | Modes | Notes |
| --- | --- | --- | --- |
| Markdown | `text/markdown`, or `.md`/`.markdown` with a text type | Preview, Source (editable) | Today's preview and textarea. Preview is the default. |
| Text | Other text by media type, extension, or no NUL byte in the first KiB (as today) | Source (editable) | No toggle: today's Markdown preview of code goes away. |
| Image | `image/*` and image extensions | Preview | Today's `<img>`. SVG as an image, never inline. |
| PDF | `application/pdf` or the PDF signature | Preview | Today's `PdfPreview`. |
| Fallback | Anything else | Info | The file's facts and Download, instead of "Binary file — no preview". |

### Later

- **Monaco** ([U109](u109-file-editor.md)): Source mode for code, HTML, CSS, JavaScript, JSON and YAML, ranked above Text. It is loaded lazily, with languages loaded on demand and its theme following the app's. It registers a serializer, and ⌘S goes to the host.
- **HTML:** Preview in a sandboxed `iframe`, plus Source through Monaco.
- **Office:** previews of Word, Excel and PowerPoint, the type of files in the demo's Presentations workspaces. Options, to choose in that plan:
  - client-side libraries (for example `docx-preview` for Word and SheetJS for Excel);
  - converting to PDF on the server, then the PDF provider;
  - never Microsoft's public viewer, which needs a public URL.
- **Image editing:** an Edit mode for crop, rotate and resize, saving bytes through the shared session.

## Phases

1. **Registry and host with today's behaviour.** Move the five first providers behind the registry and host. The visible changes are:
   - only Markdown has the toggle, and it opens in Preview;
   - the mode is remembered per file, on the device;
   - unknown files get the fallback's details.
2. **Editing session.** Move saving into `editing.ts`, with states and progress, byte saves, the three conflict choices, and the unsaved-changes guard.
3. **Loading and the blob viewer.** Add a raw-bytes endpoint, URL content and size limits per provider. Move the blob viewer onto the registry and the host, read-only, and remove `blobView()` (see [The blob viewer](#the-blob-viewer)).
4. **Monaco** ([U109](u109-file-editor.md)).
5. **HTML preview, Office previews, image editing,** each as its own provider.

## Tests

- The registry: which provider wins for a set of paths, media types and first bytes, including the fallback.
- The host: no toggle for one mode, a toggle for several, the remembered mode, and a draft kept across modes.
- The editing session: the state changes, progress, the 409 choices, and byte saves.
- The page's existing tests (`WorkspacesPage.blob-view.test.tsx`, `workspace-transfers.test.tsx`) keep passing through phase 1 unchanged, except where a test relies on code files having a preview.

## Open questions

- Should large byte saves use the existing upload endpoint or a new raw-bytes `PUT`?
- What is the text size limit, and should larger text open read-only instead of offering a download?
- Which Office approach: client-side libraries, server conversion, or both?
- Is "Edit" a separate mode from an editable "Source", or does a provider with visual editing replace Preview with Edit?
- Should the blob viewer keep showing SVG and HTML as text, or show an SVG as an image like the workspace does?

## Decisions

- The mode is remembered per file, on the device, not per provider.
- Markdown opens in Preview.
- The blob viewer picks its provider from the file name in its link, through the same file name to media type converter as the workspace.

## Progress

Phase 1 is done:

- `src/components/file-providers/` holds the types, the registry (`providerFor`, `fileFacts`), the media types by extension, the file host, the editing session (`useWorkspaceFileEditing`, today's save flow moved unchanged), and the five providers: Markdown, Text, Image, PDF and Fallback.
- `FileDetail` in `src/pages/WorkspacesPage.tsx` loads the tree and the content, keeps the editing session, and hands the rest to `FileHost`. The page's shared preview state, `classify()` and the extension tables are gone.
- A draft stays in the text editor when its file can no longer be read as text (replaced or removed), as it did before.
- Tests: `registry.test.ts` covers which provider wins. `WorkspacesPage.blob-view.test.tsx` covers Markdown opening in Preview, a draft kept across modes and saved from Preview, the mode remembered per file, no toggle for text and images, and the fallback's details.

Differences from the design, for now:

- The built-in mode views are not loaded lazily; they were already in the page's bundle. The host wraps every view in Suspense, so a provider with a heavy view (Monaco) passes a `React.lazy` component.
- Content is `text`, `bytes` or `none`. `url` waits for the raw-bytes endpoint (phase 3).
- `FileEditing` changes text content only, and has no serializer yet (phase 2 and Monaco).
- The blob viewer still has its own classifier and its own views; it moves onto the providers in phase 3 (see [The blob viewer](#the-blob-viewer)).
