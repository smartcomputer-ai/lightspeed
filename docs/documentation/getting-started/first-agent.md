# Build your first agent

A profile gives an agent a repeatable setup, and a workspace gives it files
that persist between runs. This walkthrough combines them into a release
editor: an agent that reads a short change list and writes release notes you
can inspect and revise.

Start with a running installation, a universe, and a connected model from the
[quickstart](quickstart.md). Use a universe owner/admin or platform administrator
account to manage profiles, workspaces, and sessions. The agent will use VFS
file tools, so it needs no execution environment.

## Give the agent source material

Open **Workspaces** and choose the plus button labeled **New workspace**.
Enter `Release notes` as the **Display name** and `release-notes` as the
**Workspace id**, then choose **Create**.

In that workspace, choose **New file**, enter `changes.md` as the **Path**,
and choose **Create**. Paste this fictional product change list into the file
editor and choose **Save**:

```markdown
# Changes for Acorn 1.2

- Added a CSV export button to the activity page.
- Fixed a bug that showed completed tasks as overdue.
- Renamed the "Members" settings page to "Team".
- Existing API requests and saved data are unchanged by this release.
```

The file belongs to the workspace. The next step gives a session access to it
at a particular path.

## Create a reusable profile

Open **Profiles** and choose the plus button labeled **New profile**. Set
**Display name** to `Release editor`, **Profile id** to `release-editor`,
and **Start from** to **Empty profile**. Choose **Create**.

In the profile editor, enter these **Instructions**:

```text
You write release notes from the source material supplied in the workspace.
Use only the facts in that material. Do not invent benefits or compatibility
claims. Write clear, short paragraphs for people who use the product.
Use the VFS tools to read source files and save the requested output file.
After saving, report the file path and any uncertainty that needs review.
```

Under **Model configuration → Model**, select a model from your connected
provider that supports tool calls. Select it explicitly so the profile does
not depend on a different deployment default.

Instructions describe the work, but they do not grant access to files. Enable
**Virtual File System: Files, Instructions, Skills**.

Under **Workspace attachments**, choose **Add link** and configure:

| Field | Value |
| --- | --- |
| Target type | Workspace |
| Workspace | Release notes (`release-notes`) |
| Session path | `/workspace` |
| Access | Edit |

Leave the other capabilities and prompt/skill roots unset, then choose
**Save**. The profile now links an editable workspace, and the link is what
installs the file tools: any attachment grants reading, and an **Edit** link adds
writing. A **Read** link could not accept the release notes.

The session path is how this agent sees the workspace. Its source file will
be `/workspace/changes.md`. In the workspace browser, the same file is simply
`changes.md`, because the browser already starts at the workspace root.

## Start a session from the profile

Open **Sessions**, choose **New session**, and enter `First release notes`
as the **Name**. Select **Release editor** under **Profile**, then choose
**Create**.

Send this task:

```text
Read /workspace/changes.md and write release notes to
/workspace/release-notes.md. Use the headings "New", "Fixed", and
"What to expect". Keep the whole document under 150 words.
Read the saved file back to check it, then tell me where it is.
```

The run should show file-reading and file-writing activity, followed by an
answer that identifies the saved file. The exact wording and sequence of tool
calls can vary by model. What matters is the resulting file and whether its
contents match the supplied facts.

## Inspect the work

Expand the tool activity in the session transcript. **Arguments** shows what
the agent asked the tool to do; **Result** shows what happened. A failed call
shows an **Error** instead. This lets you check the file path and outcome
without relying only on the agent's final message.

Return to **Workspaces → Release notes** and open `release-notes.md`. Check
that it describes the export button, corrected overdue status, renamed page,
and compatibility statement. The page should not add claims that were absent
from `changes.md`.

This is the first useful result: the agent produced a persistent artifact
from explicit source material, and you can inspect the operations that created
it.

## Continue the same session

Return to **First release notes** and send:

```text
Revise /workspace/release-notes.md so the opening addresses the reader as
"you". Keep the headings and all factual claims unchanged. Save the file
and briefly describe the edit.
```

This starts another run in the same session. Open the workspace file again to
inspect the edit. Refresh the session page, or leave and reopen it, to confirm
that the conversation is still available.

You can also create another session from **Release editor**. It will have a
separate conversation and the same profile setup. Because the profile links
to the existing `release-notes` workspace, it will see the same files. Use a
different workspace when you want a separate set of artifacts.

Editing the saved profile affects subsequent sessions created from it.
Existing sessions keep their setup until you explicitly change or reapply it.

## If the result is missing

| Symptom | What to check |
| --- | --- |
| The agent prints release notes in chat but never saves a file | Confirm that the task asks for a saved file and the workspace attachment has **Edit** access. Inspect the transcript for an actual write operation. |
| The agent cannot find `changes.md` | Check the workspace attachment and session path. The agent needs `/workspace/changes.md`; the workspace browser shows `changes.md`. |
| The write is refused | The workspace attachment must use **Edit** access; a **Read** link or a snapshot never accepts writes. |
| Fixing the profile does not fix the session | Create a fresh session from the corrected profile, or explicitly update the existing session's setup. |
| The file contains unsupported claims | Revise the instructions or ask for a correction against the source. Tool success verifies that a file was written, not that its contents are correct. |

Continue with [Sessions and runs](../using-lightspeed/sessions-and-runs.md) for
queueing, steering, and inspecting work, or
[Profiles and instructions](../using-lightspeed/profiles-and-instructions.md)
to build a read-only reviewer. [Workspaces and skills](../using-lightspeed/workspaces-and-skills.md)
extends this example with project instructions and a reusable review procedure.

Add [an execution environment](../environments/overview.md) when your agent
needs to run code, use command-line tools, or work with a machine's filesystem.
