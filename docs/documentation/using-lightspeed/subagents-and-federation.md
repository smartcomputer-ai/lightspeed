# Sub-agents and federation

An agent can split a task into work for specialists, or send an event to
another bot with its own ongoing job. Lightspeed provides separate mechanisms
for those relationships because they have different ownership and lifetimes.

A **sub-agent** is a child session created for a bounded task. It uses an
allowed profile, returns one result, and closes. **Bot federation** sends a
durable event to an independently configured bot in the same universe. That
bot owns its inbox, routing, conversations, and future work.

| Need | Use |
| --- | --- |
| Review a draft and return findings to the current task | A sub-agent. |
| Run several independent checks and combine their results | Multiple sub-agents, with joined calls or promises. |
| Notify an ongoing release bot that a review is ready | A federation event. |
| Ask another bot to handle work on its own schedule and report later | Federation with a requested reply receipt. |

Both mechanisms require explicit grants. Use a universe owner/admin or
platform administrator account to configure them.

## Delegate a release review

First create the `release-reviewer` profile from
[Profiles and instructions](profiles-and-instructions.md#create-a-profile-for-a-job).
It should have a clear description, read-only VFS tools, and its own read-only
link to the `release-notes` workspace at `/workspace`.

Open the parent `release-editor` profile and enable **Sub-agents**. In
**Agents**, select `release-reviewer`. Optional limits are hidden by default;
choose **Customize limits** to edit them. Set small limits for this task, for
example **Max depth** `1`, **Max descendants** `8`, **Max concurrent** `2`, and
**Deadline (ms)** `300000`. Save and start a new session from the parent
profile, or apply it to an existing idle session.

Ask the parent:

```text
Ask the release-reviewer sub-agent to compare /workspace/release-notes.md
with /workspace/changes.md. Give it both paths and ask for unsupported claims,
missing changes, and uncertainties. Wait for its result. Check its findings
against the files, then report the changes you recommend. Do not edit yet.
```

The child receives the supplied brief and its own profile. It does not receive
the parent's conversation or inherit its tool grants. Include the facts,
paths, and expected result the child needs in the brief. In this example,
both profiles explicitly link the same workspace, so both agents can read
the files without copying them into the child conversation.

Inspect the parent's tool activity and follow the **Sub-agents** link to the
child transcript. A completed child should show its own reads and review;
the parent should use that result in its answer. To find finished children in
the main session list, clear **Hide sub-agent sessions** and **Hide closed
sessions**.

## Understand the child boundary

The child runs with the selected profile's full grants. Allowing a profile
that can write to a database delegates that authority even if the parent
cannot call the database tool directly. Review the child profile as part of
the parent's access design.

Workspace links are shared only when both profiles point to the same live
workspace. They do not create isolated copies. Give a reviewer read-only
access, or use a snapshot when it must review a fixed version while another
agent continues editing.

Environment behavior is also explicit. An existing environment or an inherited
parent environment shares a real filesystem. The child profile option
**Inherit the parent's active environment (sub-agents only)** needs a parent
with an active environment and the appropriate capability.
Provisioning can give the child a separate machine, normally closed with its
session according to the selected policy. VFS files remain separate from
these machine files; see [Environments](../environments/overview.md).

A sub-agent spawned by a bot does not become another bot. It gets its profile
and brief, without the parent's bot history, inbox, or controller-specific
tools.

## Join a result or use a promise

The parent uses `agent_run` for a result returned with the tool call. Several
calls in the same model turn can run concurrently and return together.
For work that can proceed alongside other parent activity, `agent_spawn`
returns a promise that the parent can await later.

Both calls accept the same model-facing arguments. For example:

```json
{
  "agent": "release-reviewer",
  "input": "Compare /workspace/release-notes.md with /workspace/changes.md. Report unsupported claims, omitted changes, and uncertainty. Do not edit files.",
  "label": "Review Acorn release notes"
}
```

For a spawned task, the parent passes returned promise IDs to `await`:

```json
{
  "promises": ["promise_1"],
  "mode": "all",
  "timeout_ms": 300000
}
```

Use the actual returned ID. `mode: "any"` resumes when any listed promise is
ready. An await timeout stops waiting for that interval; it does not cancel
the child. `cancel` requests cancellation through the promise and closes the
child session.

Pending spawned work normally belongs to the parent run and is canceled when
that run ends. `detach` promotes a promise to session lifetime when work must
survive that boundary. The session then owns that outstanding work until it
finishes or is canceled; force-closing the session cancels it.

The result includes a status such as `completed`, `failed`, `cancelled`, or
`deadline`, plus output or error information and the child session ID. Check
the status before treating the output as a completed review. A child returns
one run's result and closes automatically; there is no child-continuation
conversation tool.

## Bound the delegation tree

The default limits are depth `2`, `16` total descendants, `4` concurrent open
descendants, and a one-hour deadline per child. The deadline ceiling is
24 hours. Configure smaller values when a task should be quick and shallow.

These limits apply through the root's delegation tree. **Max descendants** is
a lifetime count, not a slot returned whenever a child finishes. **Max
concurrent** limits open descendants. Nested grants can narrow the limits but
cannot widen the limits already pinned by their origin.

The child records its profile revision, parent, and root, so its execution can
be inspected against the setup it received. Changing the saved profile does
not rewrite a child already running.

## Connect two independent bots

Suppose `release-watch` should ask a separate review bot for help. Create a
bot with ID `release-reviewer` using the shared profile of the same name,
following [Bots and triggers](bots-and-triggers.md). The bot and profile are
different records even when their IDs match. Give the bot a brief describing
how to review incoming requests and resolve their events with findings.

Open `release-watch` and choose **Settings → Other bots**. Enable **Can
message other bots** and save. Check **Accepts messages from** as well: enabling
sending can also open an otherwise disabled inbox in the form. Set the
receiving policy you actually intend.

On the receiving `release-reviewer` bot, set **Accepts messages from → Only
these bots**, select `release-watch`, and save. **Nobody** disables acceptance;
**Any bot here** accepts senders in this universe. The recipient owns its
inbox's **Routing & batching…** settings. Keep queue delivery for a review
that needs its own completed run.

Give each bot a useful description. The sender's directory lists enabled bots
in the same universe whose inboxes accept it. Sending permission takes effect
at the sender's next idle boundary; receiving policy applies to the next
event.

Send a test event to `release-watch` asking it to request a review from
`release-reviewer` and request a reply. Its `bot_emit` tool can submit:

```json
{
  "to": "release-reviewer",
  "kind": "release.review",
  "summary": "Review the Acorn 1.2 release notes against the change list.",
  "data": {
    "changes": "/workspace/changes.md",
    "notes": "/workspace/release-notes.md"
  },
  "reply": true
}
```

The receiving profile must independently have access to those paths. Event
data supplies references and facts; it does not mount the sender's files or
grant its capabilities.

## Follow admission and replies

`bot_emit` returns the destination bot and admitted event sequence number.
That means the recipient accepted the event, not that the review finished.
Inspect the recipient's Activity and the conversation that handled the event
to follow its work.

With `reply: true`, the controller can later send a `bot.reply` receipt to the
original logical sender conversation. The receipt is correlated by bot and
event sequence. The recipient should resolve the event with a useful outcome
and summary; the sender handles that later receipt as another event. There
is no cross-bot promise to await like a sub-agent result. When the sender's
current event depends on that reply, it can resolve the event as `deferred`
and handle `bot.reply` in a later run. Deferral itself does not schedule a retry.

Receipts follow the recipient's delivery semantics. Append or steer delivery
can acknowledge that disposition rather than a completed answer. Receipts
can also be skipped when the requester is disabled or closed, or the hop
limit is reached. Design the bot's brief to handle missing or deferred replies
without claiming the requested work completed.

Federation is confined to one universe. Rate limits and an eight-hop ceiling
bound chains between bots. A send can be refused because the target is
unavailable, has no accepting inbox, filters the event, or reaches a rate or
loop limit. Messaging permission does not let one bot create or configure
its neighbors.

## If delegated work is missing

| Symptom | What to check |
| --- | --- |
| The parent cannot find a specialist | Add the named profile to **Sub-agents → Agents** and give it a useful description. |
| The child cannot read the parent's files | Configure links in the child profile; the brief alone grants no access. |
| New children are refused despite none currently running | Check the lifetime descendant budget as well as concurrency and depth. |
| A spawned child ends before its result is used | The parent run may have ended with a run-scoped promise still pending. Await it or deliberately detach it. |
| A bot is absent from the federation directory | Check sending permission, target state, and the recipient's enabled inbox allowlist. |
| A send succeeded but there is no answer | Admission and completion are separate. Inspect receiver delivery, outcome, pause/budget state, and requested-reply limits. |
