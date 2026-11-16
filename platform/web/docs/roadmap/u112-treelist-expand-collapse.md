# U112: Tree List Expand and Collapse

Status: [in progress]

Lists with nested rows (the bots list, the sessions list, a workspace's files) should let people open and close each row's children, without giving up the status dots and tree lines that show state and structure.

## Expand modes

How a list's rows open and close is a setting of the list, apart from what the list holds (its `mode`). `ListPaneBody` (or `ListLayout`) takes `expand`, typed `ListExpand` in `src/components/list-pane.tsx`:

| Mode | Behaviour |
| --- | --- |
| `none` | The default. Rows do not open and close; a tree shows whole. |
| `dedicated` | A caret of its own before a row's content opens and closes its children, while clicking the row still opens its detail. |
| `title` | As `dedicated`, but the caret sits in the title line, right after the title's text. |

More types can join `ListExpand` later. The files list's folders, where clicking the whole row opens and closes it (`ListRow toggle`), are a candidate for one.

## Dedicated mode

- A row with children passes `expander={{ open, onToggle, label? }}`. The caret points right while closed and turns down while open.
- The caret is a button of its own beside the row's link, never inside it, so the row stays a link. It sits at the height of the status dot, has a 20px target over its 16px room, a tooltip, and `aria-expanded`. Hovering it keeps the row lit.
- On a focused row, the right arrow opens and the left arrow closes.
- Status dots and tree lines stay where they are. The caret takes room at the start of the content, moving the title and any avatar over.
- **Caret room per sibling group.** The rows under one parent (or the roots) keep the caret's room when any of them can open, so their titles line up; they pass `caretRoom`. A group where none can open takes no room and stays to the left.
- The list draws only the rows it is given: the caller leaves a closed row's children out and sets `tree.children` only while the row is open.
- In files mode, a folder's open and closed icon follows its caret as it follows its toggle.

## Title mode

Title mode works as dedicated mode (its own caret button, the arrow keys, the caller deciding what is open), but the caret follows the title's text in the title line. Since it moves with the text, rows keep no room for it and `caretRoom` does nothing; a long title truncates before its caret. The caret stays a button beside the row's link: the row keeps a small room after the text, and the caret is placed over it, measured again whenever the row or its title changes size.

## UI overview

The Lists page shows every mode: the bots list and the file list each have an Expand switch (Off, Dedicated, Title), and two further examples show dedicated rows with avatars (the bots list) and without (a sessions list).

## The bots list

The bots list (`src/pages/BotsPage.tsx`) opens and closes its rows by caret. It uses title mode for now, to compare it with dedicated mode in use; the behaviour below holds in either. Each bot's row stands for its Main conversation; its sub-rows are its threads, most recently active first, and each sub-agent under the conversation that delegated to it (one of Main's under the bot itself).

- **What opens by default.** Selecting a bot opens it one level down: its threads and Main's own sub-agents. A thread's sub-agents stay closed. When the selected conversation is deeper (a deep link, or a pick from the header's sub-agent popover), the rows above it open once, so it is in sight. Rows opened or closed by hand stay so while the list is shown.
- **Which rows get a caret.** A bot whose conversations have been read gets one when they include anything besides Main. Before they are read, the bots list's `openSessionCount` decides: more than one open session means more than Main. Without either, whether it has children is not known, and it gets no caret. A thread gets one when it has sub-agents.
- **What is read, and when.** A bot's conversations come from its state (`GET /bots/{id}/state`), read only while its row is open. The selected bot's page reads the same query and keeps it fresh, so selecting a bot adds no request. Another open bot refreshes every 15 seconds and stops when closed. The list itself reads nothing more than the bots list.
- The demo backend sends `openSessionCount` and `activeSessionCount` on each bot, as the server does.

Tests: `src/pages/BotsPage.tree.test.tsx` covers the default opening, which bots offer a caret, that another bot's state is read only once opened and no longer refreshed once closed, and the path to a deep link. `src/components/list-pane.test.tsx` covers the caret, its room, and the arrow keys.

## The sessions list

The sessions list (`src/pages/SessionsPage.tsx`) uses title mode too. Each session with sub-agents gets a caret after its title; its sub-agents nest under it, and theirs under them.

- **What opens by default.** Selecting a session opens it one level down, and the rows above it, so a selected sub-agent is in sight, once per selection and as soon as it is loaded. Every other session starts closed. Rows opened or closed by hand stay so while the list is shown.
- **What is read.** Nothing more: the list already reads each session with its sub-agents, and builds the tree from them, so every caret is known from the start.

Tests: `src/pages/SessionsPage.tree.test.tsx` covers the carets one level at a time and what a selection opens.

## Open work

- **Keeping open rows.** Open rows are forgotten when the list unmounts. Keep them per universe, with the other display preferences, and save them with the account once settings are saved on the server (U108).
- **Counts that include closed conversations.** `openSessionCount` counts open sessions only, so a bot whose only conversations besides Main are closed sub-agents gets no caret until its state is read (selecting it reads it). A count of all conversations, or of threads and sub-agents apart, on the bots list would close that gap.
- **Long lists of children.** A bot with many threads lists them all once open. Consider showing the most recent few with a row that shows the rest.
- **Keyboard navigation.** Up and down move through the rows as links do today. A full tree pattern (`role="tree"`, moving to a parent with the left arrow on a closed row) is not done.
- **Reduced motion.** The caret's turn is a transition; it should follow the reduced-motion setting (U110).
