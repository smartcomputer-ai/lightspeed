# Documentation site

Astro Starlight renders the product manual at `/docs/`. The site is a separate
npm workspace within Lightspeed. It builds static files and does not need the
Lightspeed runtime, credentials, a database, or the neighboring `ls-site` repo.

From the repository root:

```bash
npm install
npm run dev:docs       # local preview; open the printed URL under /docs/
npm run check:docs     # adapter tests, Astro diagnostics, build, and link checks
npm run build:docs     # static output in docs/site/dist/
npm run preview:docs   # serve the production build, including search
```

Astro's development server watches the original manual and the included
references. Editing a source updates the preview. Astro 7 runs the development
server in the background; `npm exec --workspace @lightspeed/docs -- astro dev stop`
stops it. Search is indexed during the production build, so use `preview:docs`
when checking search behavior.

## Authoring and navigation

Edit ordinary Markdown in `docs/documentation/`. Keep its leading `# Title`,
relative Markdown links, code fences, and Mermaid diagrams readable in GitHub.
`scripts/content.mjs` derives Starlight title/description metadata, removes the
duplicate body title, and adapts links into ignored `.generated/` files. Those
files are build inputs, not an additional source to maintain.

Add completed pages to the sidebar in `astro.config.mjs`. Keep implementation
roadmaps and unfinished placeholders outside the published manual. The home
page's four reading-path cards are in `src/components/MarkdownContent.astro`.

The environment-variable reference lives in the manual. Two generated
references are included directly from their authoritative sources:

| Published route | Source |
| --- | --- |
| `/docs/reference/environment-variables/` | `docs/documentation/reference/environment-variables.md` |
| `/docs/reference/api/` | `crates/api/contract/api-reference.md` |
| `/docs/reference/workflow-contract/` | `crates/temporal-workflow/contract/workflow-contract.md` |

Regenerate contracts with their existing Rust exporters. Do not edit generated
contracts or their staged copies. Other repository links point to GitHub;
relative images are copied into the output so the site can serve them locally.
Broken repository paths fail the build. The output check validates site links,
heading anchors, local assets, canonical URLs, diagram transforms, and the
presence of the search index and sitemap.

## Documentation screenshots

UI screenshots live in `docs/documentation/images/` and use relative Markdown
image links from the pages they illustrate. Keep one image at the relevant
procedure, with descriptive alt text and a short caption identifying demo data.
Crop to the dialog or working panel so labels remain readable at article width.

To refresh them, run `npm run demo`, open `http://localhost:5175/demo/`, and use
the **Software Factory** universe. Select **Dark** under the account menu's
**Theme** submenu, hide the demo notice, and use a 1440 × 1000 viewport at
normal zoom. Use Playwright to operate the UI and capture PNGs with
`scale: 'css'` and `animations: 'disabled'`. Wait for the relevant view to load,
move the pointer away from controls, and use a dialog or panel crop. Do not
replace UI text or fabricate results. These captures need no live backend or
credentials.

For a loopback-only demo server, use:

```bash
npm run dev:demo --workspace @lightspeed/platform-web -- --host 127.0.0.1
```

| Image | Demo view and state |
| --- | --- |
| `welcome-session.png` | Sessions → Fix flaky scheduler test; expand the first run's activity and scroll the conversation to the top. Leave individual tool calls collapsed. Capture the full UI at 1440 × 960, including navigation and the sub-agent link. |
| `new-session.png` | Sessions → New session; name `First conversation`, no profile, before customizing. Capture the dialog. |
| `profile-editor.png` | Profiles → Release scribe → Form; capture the header, instructions, and model configuration, with Customize run controls closed. |
| `session-tool-result.png` | Sessions → Fix flaky scheduler test; expand the first run, then its Run tool call. Capture the tool batch with the first command's Result selected. |
| `workspace-skill.png` | Create the `release-notes` workspace and save the skill from the Workspaces and skills guide at `.lightspeed/skills/release-review/SKILL.md`. Capture the tree and editor. |
| `bot-event-outcome.png` | Bots → PR Reviewer → Activity; expand event #11. Capture the bot header, activity controls, expanded event, and neighboring outcomes. |
| `environment-details.png` | Environments → CI runner → Details; capture the expanded machine card. |
| `members-and-roles.png` | Members; capture the heading, Add member button, and member table showing the seeded Admin and Contributor roles. |
| `session-share.png` | Fix flaky scheduler test → Session actions → Share with universe…; capture the confirmation, then cancel. |
| `api-key-groups.png` | API keys → Create key; enter `Release automation`, keep Agent client selected, and capture the dialog including its private-session warning. Cancel without creating a key. |
| `model-providers.png` | Models → Add provider; capture the provider catalogue before selecting a connection. |

The release-notes workspace is created through the demo UI for this capture;
it is not a seeded fixture. Refreshing the demo discards that work. Other
captures use the existing fixtures. Captions distinguish these examples from
the reader's installation. Run the docs build after replacing images to check
their published paths and Markdown exports. Preview the built pages in
Playwright at desktop and mobile widths; check image loading, readable crops,
and horizontal overflow.

## Markdown access for agents

The same build exports one Markdown file per published page, plus an index:

| Resource | URL |
| --- | --- |
| Documentation home | `/docs/index.md` |
| Individual page | `/docs/getting-started/quickstart.md` |
| Page index with descriptions | `/docs/llms.txt` |

The exports preserve the original H1, Markdown formatting, code examples, and
Mermaid source. Links to published pages point to their Markdown URLs; image
URLs are absolute and point to bundled assets. Repository source links still
lead to GitHub. Starlight frontmatter and navigation markup are excluded.
Only pages in the published manual and its included references are exported.

Every HTML documentation page advertises its Markdown URL with
`<link rel="alternate" type="text/markdown">`. The 404 page has no Markdown
alternate. Source edits, renames, and removals update the exports and index
alongside the HTML build. These files are generated into the public assets,
so both local preview commands serve the direct `.md` and `llms.txt` URLs.

### Content negotiation at deployment

The static bundle supplies both representations. The infrastructure's Caddy
configuration must choose the representation at request time; Astro middleware
does not run when Caddy serves this static output. Header negotiation is not
enabled by the build or the local Astro preview.

For existing documentation pages, the serving contract is:

| Request | Representation |
| --- | --- |
| `/docs/` with `Accept: text/markdown` | `index.md` |
| `/docs/getting-started/quickstart/` with `Accept: text/markdown` | `getting-started/quickstart.md` |
| A page with `Accept: text/html`, an absent header, or `*/*` | Its HTML directory index |
| A direct `.md` URL | The Markdown file |
| `/docs/llms.txt` | The Markdown discovery index |
| Unknown page or Markdown URL | A 404 response |

Apply negotiation to page routes, using the existing route's matching `.md`
file. Parse media ranges and quality values: `text/markdown;q=0` must not
select Markdown, and `text/html;q=1, text/markdown;q=0.5` must select HTML.
An explicit Markdown preference selects Markdown; unqualified wildcards
default to HTML. Serve the selected file at the requested page URL with a
200 response, rather than redirecting to another representation.

Markdown responses, including direct `.md` requests, use
`Content-Type: text/markdown; charset=utf-8`; `llms.txt` uses
`text/plain; charset=utf-8`. Both HTML and Markdown responses on negotiated
page URLs include `Vary: Accept`. Any cache in front of Caddy must distinguish
the variants. Normal canonical-path redirects, including `/docs` to `/docs/`,
continue to apply. The origin can also point `/llms.txt` to `/docs/llms.txt`
to expose the documentation index from the site root.

After wiring the deployment, verify the same page with both representations:

```bash
curl -i -H 'Accept: text/markdown' https://ls.bot/docs/getting-started/quickstart/
curl -i -H 'Accept: text/html' https://ls.bot/docs/getting-started/quickstart/
curl -i https://ls.bot/docs/getting-started/quickstart.md
curl -i https://ls.bot/docs/llms.txt
```

## Appearance and assets

`src/styles/theme.css` adapts the landing page's Merriweather headings,
Merriweather Sans body text, League Gothic wordmark, and cyan/cobalt/lavender
palette. Dark mode uses graphite surfaces and cyan accents; light mode uses
white surfaces and cobalt links. Starlight supplies the light/dark/automatic
selector, responsive navigation, keyboard search, and code-copy controls.
Dark-mode body text uses the hero's light weight (300), with medium-weight
emphasis (500) and regular serif headings (400). Near-white body text, white
headings, and brighter secondary text preserve contrast on dark surfaces.

All three fonts are preloaded in the document head using the same asset URLs
as the stylesheet. `font-display: block` briefly waits for a font on a cold
load instead of flashing fallback typography before swapping to the brand
face. League Gothic is used for the wordmark and section labels; Merriweather
is used for page and section headings, and Merriweather Sans for body text.

The fonts were copied from `ls-site/public/fonts/` and live under
`src/assets/fonts/`. Their SIL Open Font Licenses from the Google Fonts
repository are shipped in `public/licenses/`. The transparent white mark was
copied from `ls-site/public/assets/ls-logo-2026-v1-ls.svg`; its black-stroke
inverse is used in light mode. The favicon is the same white mark served by
the landing page, copied into `public/assets/`. `public/social.png` is the existing
`docs/images/ls-screenshot-factory.png`. These are checked-in assets: no build
step reads `ls-site`, and no font service is contacted by readers.

The explicit `cookie` dependency matches Astro's prerenderer. It keeps static
builds resolving the correct API in this workspace, which also contains older
cookie versions used by Express and React Router.

## CI and hosting

The `docs` job in `.github/workflows/ci.yml` runs when the published manual,
site, images, included references, or shared build inputs change. The shared
`required` gate waits for every selected suite. `check:docs` is separate from
the root product `check` command; `npm test` still runs all workspace tests,
while the consumer CI job uses `test:consumers` to exclude docs tests.

Every main snapshot and tagged release builds the site alongside the product
and packages `docs/site/dist/` as `lightspeed-docs-<version>.tar.gz`. The archive
has `index.html` at its root and includes pages, CSS, JavaScript, fonts, images,
licenses, Pagefind search, the sitemap, `404.html`, per-page Markdown, and
`llms.txt`. It contains no source workspace or server runtime. The release
manifest's `artifacts.docs` entry
records its filename, SHA-256 checksum, `/docs/` base path, and immutable
`oci://.../docs-bundle@sha256:...` location. It is also included in the overall
release bundle, checksums, and build provenance. Tagged releases publish the
archive on GitHub and assign `docs-bundle:<version>` in the registry.

The existing main-build notification sends the release-bundle digest. A
deployment can read `artifacts.docs` from that manifest and install the bundled
archive or fetch its exact OCI reference. No separate docs notification is
required. Docs-only main pushes run the selected docs checks and then publish
a complete snapshot, so documentation corrections reach the same build stream.

The output directory is the document root for `https://ls.bot/docs/`:
`dist/index.html` serves `/docs/`, not `/docs/docs/`. Configure Caddy to strip the
`/docs` prefix before looking up files, redirect `/docs` to `/docs/`, serve
directory indexes, and return `404.html` with a 404 status for unknown paths.
Assets and search URLs already include `/docs/`. Add the docs sitemap to the
origin's existing `robots.txt` when publishing.

The manual is labeled **Current development** and records its source Git SHA
in page metadata. The deployment installer and public Caddy route remain work
for the infrastructure repository. Publishing these resources does not deploy them.
