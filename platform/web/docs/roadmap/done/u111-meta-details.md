# U111: Meta Details

Status: [done]

Meta toolbar items open a popover on click when they carry `details`. The popover itself is shared: `MetaItemView` in `src/components/meta-toolbar.tsx` wraps every item's `details` in the same `PopoverContent` (surface, notch, width, padding, open and close). What goes inside it is not shared. Each caller builds its own content, and the copies only look alike because their classes were copied by hand:

| Content | Where | Built with |
| --- | --- | --- |
| Environment, Files, MCP servers, Sub-agent profiles, Code mode, Web, Timers | `src/components/session/session-resources.tsx` | Private helpers in that file: `Heading`, `Row`, `Empty`, `Note`, `Footer` |
| Sub-agents | `src/components/session/session-meta.tsx` (`SubAgentList`) | Its own markup: a copied heading, link rows with the real `StatusDot` |
| A workspace file's content | `src/components/workspace-file-meta.tsx` (`FileContentDetails`) | A copied heading and row |
| UI overview examples | `src/devtools/meta-toolbar-examples.tsx` (`Details`) | A fourth copy |

They already differ: resource rows use a small plain dot (`TONE_DOT`), sub-agent rows use `StatusDot` with its rings, and only the sub-agent rows are clickable.

## Proposal

Create one set of parts for the content of a meta item's popover, in `src/components/meta-details.tsx`:

- **Heading:** a title and a muted line saying what the item is.
- **Row:** primary text, secondary facts joined by ` · `, optional badges, and a status dot. With an `href` the row is a link that highlights on hover and focus like a menu item and closes the popover when followed (`PopoverClose`). Without one it is plain text.
- **List:** the rows, scrolling inside the popover past a maximum height, never adding a second scroll bar beside the popover's own.
- **Empty:** the line shown when there is nothing to list ("No environments are attached.").
- **Note:** a separated muted line under the rows.
- **Footer:** the actions under a separator: change in session settings, open the related page, load more.

Every row uses the same status dot. Use `StatusDot` with the existing status tones and map the resource tones (`ok`, `idle`, `busy`, `problem`) onto them, so an environment and a sub-agent in the same state look the same.

Then move every caller onto these parts: the session resource popovers, the sub-agent list, the workspace file content, and the UI overview examples. Their content stays the same; only the shared look and behaviour change.

Add a section to `/debug/ui-overview` that shows the parts on their own: a heading, plain and linked rows in each tone, badges, an empty state, a note, a footer, and a long list that scrolls.

## Tests

- The parts: a linked row closes its popover when followed, and a plain row is not focusable.
- The existing popover tests (the session resources, the sub-agent list in `src/pages/SessionsPage.lineage.test.tsx`, the workspace file meta) keep passing unchanged, or are updated only where the markup they query has moved.
