# P114: Documentation Structure And Website

**Status**

- Proposed 2026-08-03. Not started.
- Greenfield restructure. `docs/` becomes the published documentation website;
  no compatibility paths or duplicate content trees are preserved.

## Goal

Make a documentation website the canonical source for user-facing
documentation, keep engineering records separate, and keep the whole thing
small: roughly 12–15 published pages at launch.

## Repository Restructure

- `docs/` becomes an Astro Starlight site in full. Nothing under `docs/` is
  unpublished working material.
- `docs/roadmap/` moves to a top-level `roadmap/`. It stays unpublished.
- `docs/spec/` is deleted. It is vestigial; git history retains it. Dangling
  textual mentions inside historical roadmap files are acceptable and are not
  rewritten.
- `docs/images/` relocates into the site: content images to `docs/src/assets/`
  (optimized by Astro), logo/favicon to `docs/public/`.
- Live references to update in the same commit: `AGENTS.md` (index entry and
  maintenance rule), `README.md` (three image paths), and
  `crates/engine/README.md`.

Target layout:

```text
README.md
roadmap/                      # engineering records, unpublished
docs/
├── package.json
├── astro.config.mjs          # sidebar defined here
├── public/                   # logo, favicon
└── src/
    ├── assets/               # content images
    └── content/docs/
        ├── index.mdx         # splash homepage
        ├── start/
        ├── concepts/
        ├── guides/
        └── reference/
```

## Information Architecture

Four sidebar sections plus a Contributing top-nav link (thin page: build/test
basics, link to `AGENTS.md` for the crate map and rules — no duplication).

```text
Start here
├── What is Lightspeed?
├── Installation
├── Run your first agent
└── Next steps

Concepts
├── Architecture            # short orienting page that links out
├── Sessions and runs
├── Deterministic execution
├── Context and compaction
├── Tools and capabilities
├── VFS and environments
└── Durable workflows and sub-agents

Guides
├── Run Lightspeed locally
├── Configure an agent
├── Create and use profiles
├── Connect an MCP server
└── Use environments

Reference
├── Configuration
├── CLI
├── JSON-RPC API            # generated, never hand-edited
├── Environment variables
└── Crate map
```

Deferred guides (second wave, not v1): workflow-backed tools, multi-tenancy,
deployment.

## Content Mapping

- README "Why?"/features → homepage and "What is Lightspeed?".
- README quick start → `start/`; local runtime section → `guides/run-locally`.
- `docs/design.md` → `concepts/`. Its seven sections map near one-to-one onto
  the concept pages; `concepts/architecture` keeps only the intro narrative and
  diagram and links out. One canonical explanation per concept.
- `interop/contract/api-reference.md` → `reference/api`, copied by a site
  `prebuild` step. `cargo run -p api --bin export-schema` remains the only
  generator; `cargo test -p api` already fails when artifacts are stale.
- CLI reference generated from clap where practical, same one-generator rule.
- README shrinks to: one-paragraph description, architecture image, three to
  five capabilities, five-minute quick start, links to the site.

## Rules

- Organize pages around reader tasks, not crates.
- Guides link to concepts; they do not repeat explanations.
- Generated documentation is never edited manually.
- Roadmap files are never the primary public explanation of a feature.
- Anything documented as working must be exercised by tests or CI.

## Website Stack

Starlight defaults: default theme, logo plus one accent color, local search
(Pagefind), "Edit this page" links, GitHub link, splash homepage. Static
deploy from `main` (GitHub Pages via Actions, or Cloudflare Pages if PR
previews are wanted). CI on PRs: build the site and validate internal links.

Explicitly out of scope for v1: versioning, blog, CMS, analytics,
localization.

## Slices

1. Restructure: move `roadmap/` to top level, delete `docs/spec/`, relocate
   images, update `AGENTS.md`, `README.md`, `crates/engine/README.md`.
2. Scaffold Starlight in `docs/` with homepage, "What is Lightspeed?", and
   the run-locally guide; deploy pipeline live from the start.
3. Wire the generated API reference copy step; add environment-variable and
   crate-map reference pages.
4. Split `docs/design.md` into the concept pages; delete `design.md`.
5. Remaining v1 guides and start-here pages; shrink the README.
