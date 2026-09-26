# Integrate durable workflow tools

A workflow tool lets an agent hand work to another durable workflow and, when
needed, wait for its result. The session records the invocation and completion
relationship; the receiving workflow owns the domain work. This is useful
when a tool starts a review, coordinates several services, or must survive
longer than one HTTP request.

There are two separate integration directions. An application workflow can
call the ordinary Lightspeed API from an activity, as described in the
[API guide](api-and-typescript.md). A model can also call a declared workflow
tool, causing Lightspeed to signal an existing workflow or start one from a
stored recipe. Both use public contracts; custom activities do not need
to be installed on a Lightspeed session worker.

## Define the participants

The session workflow holds the agent state and any completion promises. A
tool receiver handles one declared kind of domain work. An optional lifecycle
controller coordinates the session itself, including when to submit new runs
or replace a closed session. The lifecycle controller and tool receiver can
be different workflows.

`session/managed/start` creates a session with immutable workflow declarations.
The declarations live outside ordinary `SessionConfig`. They identify the
function the model sees, its schema, its receiver or start recipe, and the
meaning of completion.

The current combinations are:

| Target and delivery | Allowed completion | Meaning |
| --- | --- | --- |
| Existing receiver, `bound` + `pull` | `accepted` | Record an invocation for a consumer following the event stream. |
| Existing receiver, `bound` + `push` | `accepted`, `joined`, or `promises` | Signal the receiver and optionally await its completion. |
| New execution, `start` | `joined` or `promises` | Start a workflow for this invocation and track its completion. |

`accepted` means admission only. A receiver failing later cannot turn the
already accepted tool result into a completed business operation.

`joined` parks the original tool call on one runtime-owned `reply` promise,
then resumes it when that promise resolves. It requires a positive deadline.
The agent experiences a tool call returning its eventual result; this alone
does not expose model-controlled concurrency tools.

`promises` returns handles the model can await, cancel, or detach from. Keys
can be the single `reply` key or derived from validated arguments using the
declared key source. The declaration limits how many promises may be created.
Choose this mode when the agent should continue or coordinate several results
independently.

## Declare a review tool

Consider a release pipeline whose agent requests a durable review. The custom
worker implements `ReleaseReviewWorkflow` on a `release-integrations` task
queue in the deployment's Temporal namespace. The runtime can start that
workflow through its generic start adapter.

First store the input schema and start recipe in the same universe's CAS.
The following function runs in an application process or Temporal activity,
using a `LightspeedClient` with `session` and `blobs/put` access in that universe:

```ts
import { LightspeedClient } from "@lightspeed-ai/agent-client";
import { recipeFingerprint } from "@lightspeed-ai/agent-client/workflow";

export async function createReviewSession(client: LightspeedClient) {
  async function putJsonBytes(json: string): Promise<string> {
    const stored = await client.call("blobs/put", {
      blobs: [{ bytesBase64: Buffer.from(json).toString("base64") }],
    });
    const blob = stored.result.blobs?.[0];
    if (!blob) throw new Error("Blob upload returned no reference");
    return blob.blobRef;
  }

  const schema = JSON.stringify({
    type: "object",
    properties: { release: { type: "string" } },
    required: ["release"],
    additionalProperties: false,
  });
  const recipe = JSON.stringify({
    workflowType: "ReleaseReviewWorkflow",
    taskQueue: "release-integrations",
  });
  const inputSchemaRef = await putJsonBytes(schema);
  const recipeRef = await putJsonBytes(recipe);

  return client.call("session/managed/start", {
    sessionId: "acorn-managed-review",
    profile: { kind: "named", profileId: "release-editor" },
    workflowTools: {
      version: 1,
      tools: [{
        definition: {
          toolId: "request-review",
          revision: 1,
          semanticType: "release.review.v1",
          tool: {
            name: "request_review",
            parallelism: "parallelSafe",
            kind: { type: "function", inputSchemaRef },
          },
        },
        target: {
          type: "start",
          start: {
            recipeFormat: 1,
            revision: 1,
            recipeRef,
            recipeFingerprint: recipeFingerprint(recipe),
          },
        },
        completion: { type: "joined", deadlineAfterMs: 300_000 },
      }],
    },
  });
}
```

This uses the existing release-editor profile's model and workspace setup.
The added function accepts a release identifier, such as `Acorn 1.2`. Its
parallelism declaration is appropriate only if the review implementation can
safely execute alongside other tool calls.

The recipe fingerprint covers the exact uploaded bytes. Reformatting the JSON
changes it. Keep the bytes and fingerprint together, and deploy the named
workflow on the named queue before letting a model call the tool. The recipe
does not install worker code.

## Implement the started workflow

The runtime supplies one `WorkflowToolStartArgs` argument, including the
derived execution ID, holder workflow ID, universe, and invocation. Public
API JSON uses camelCase; these workflow envelopes use their contract's
snake_case field names.

Here is a workflow-side skeleton. Its `reviewRelease` activity is application
code: it reads the argument blob through the authenticated universe API,
validates the release request, performs an idempotent review, uploads a JSON
result, and returns its blob reference. Implement and register that activity
on your custom worker before using this workflow.

```ts
import {
  ApplicationFailure, defineQuery, getExternalWorkflowHandle,
  proxyActivities, setHandler, workflowInfo,
} from "@temporalio/workflow";
import {
  DELIVER_EMISSION_SIGNAL, REPLY_COMPLETION_KEY,
  WORKFLOW_TOOL_RECOVERY_QUERY, replyPromiseId, sourceResolutionEnvelope,
  type PromiseResolution, type WorkflowToolRecoveryResult,
  type WorkflowToolStartArgs,
} from "@lightspeed-ai/agent-client/workflow";

const activities = proxyActivities<{
  reviewRelease(input: {
    universeId: string; argumentsRef: string; invocationId: string;
  }): Promise<string>;
}>({ startToCloseTimeout: "2 minutes" });

export async function ReleaseReviewWorkflow(args: WorkflowToolStartArgs) {
  const invocation = args.invocation;
  if (workflowInfo().workflowId !== args.execution_id ||
      args.universe_id !== invocation.session_universe_id ||
      invocation.tool_id !== "request-review" ||
      invocation.semantic_type !== "release.review.v1" ||
      invocation.schema_revision !== 1) {
    throw ApplicationFailure.nonRetryable("Unexpected review invocation");
  }
  const promiseId = replyPromiseId(invocation);

  const resolutions: Record<string, PromiseResolution> = {};
  const recovery = defineQuery<WorkflowToolRecoveryResult>(WORKFLOW_TOOL_RECOVERY_QUERY);
  setHandler(recovery, () => ({ resolutions }));

  const resultRef = await activities.reviewRelease({
    universeId: args.universe_id,
    argumentsRef: invocation.arguments_ref,
    invocationId: invocation.invocation_id,
  });
  const resolution: PromiseResolution = { kind: "resolved", payload_ref: resultRef };
  resolutions[REPLY_COMPLETION_KEY] = resolution;

  const reply = sourceResolutionEnvelope({
    universeId: args.universe_id,
    producerWorkflowId: args.execution_id,
    holderWorkflowId: args.holder_workflow_id,
    promiseId,
    resolution,
  });
  await getExternalWorkflowHandle(args.holder_workflow_id)
    .signal(DELIVER_EMISSION_SIGNAL, reply);
  return { resolutions };
}
```

The activity must select credentials for an authorized universe and verify
that its request belongs there. Do not place a runtime key in workflow inputs
or histories. Use `invocationId` to deduplicate the domain operation: Temporal
can retry an activity after an ambiguous outcome, even though workflow replay
itself is deterministic.

The skeleton demonstrates the successful path. Add domain-specific failure
results, cancellation, and resource cleanup to the activity/workflow. Store
each final resolution under its **completion key**, such as `reply`, before
sending it. The recovery query uses those keys, not the session's promise IDs.

The runtime monitors started executions and queries normally completed ones
as a backstop when a completion signal was not observed. A completed workflow
missing the required result fails that completion. A failed, canceled, or
terminated execution fails any still-pending completions; that path does not
recover its query results. Already resolved promises remain terminal. Resolve
all required keys and preserve recovery results across any continue-as-new
boundary.

Once all completion keys are terminal, the runtime can request cancellation
of the started execution, including after a successful resolution. Arrange
required cleanup before publishing the final result; do not rely on a long
epilogue after the last reply.

## Use an existing receiver instead

For a long-lived review coordinator, replace the start target with a bound
push destination:

```json
{
  "type": "bound",
  "dispatch": "push",
  "receiver": {
    "workflowId": "acorn-review-coordinator",
    "workflowKind": "ReleaseReviewReceiverWorkflow"
  }
}
```

Start that receiver on your own worker first. `workflowKind` pins descriptive
identity; declaring it does not register or start an implementation.

Register a handler for `DELIVER_EMISSION_SIGNAL` using the generated
`EmissionEnvelope` type and `parseEmissionEnvelope`. For `tool_invocation`,
retain `emission_id`, inspect the invocation's universe/session/tool/revision
and binding identity, and enqueue the domain work. Read `arguments_ref` in an
activity. Reply through `sourceResolutionEnvelope` to the supplied
`holder_workflow_id`, using the receiver's actual workflow ID as producer.

Persist deduplication state and queued work across continue-as-new. A signal
handler can receive a duplicate invocation; use durable identity rather than
starting the domain effect again. The parser checks wire shape, not whether
the sender is authorized for your business operation.

For `bound` + `pull`, consume `workflowToolEmitted` through the public session
event stream, maintaining a cursor and deduplication state. This mode supports
`accepted` completion only. There is no public receiver-filtered
`read_tool_emissions` RPC.

## Add lifecycle coordination when needed

A separate `lifecycleController` can be declared at managed-session creation
with its `workflowId` and `workflowKind`. It owns the application's policy
for submitting runs, coordinating other work, and retiring or replacing the
session. Tool receivers do not acquire that responsibility automatically.

For a session with that controller, `session/runs/start` can include
`notifyOnTerminal: { token: "acorn-review-001" }`. Use `client.call` for this
field; the convenience `startRun` helper does not expose it. The destination
comes from the immutable controller declaration, while the token lets the
controller correlate and deduplicate terminal notifications.

Retain the submission ID and the same notification token on retries. Terminal
emissions include the run status and references to output or failure material.
Decode the output according to its media type/provider kind. Reconcile current
run state when a notification is missed; a controller should not depend on
unbounded notification redelivery.

A controller waiting for a run must still be able to handle tool calls routed
back to itself. Otherwise both sides wait on one another. Promise-bearing
calls to the lifecycle controller require a hard deadline, and their handlers
must progress independently of the wait for the run.

## Preserve declaration and delivery semantics

Managed creation validates tool identities, name collisions, schemas, and
recipes before admitting the declarations. Retrying the same session ID
requires the same admitted declaration; changing receiver, schema, recipe,
or completion behavior requires a new session. An ordinary session cannot be
upgraded into a managed one through configuration replacement.

A tools-only declaration may omit the lifecycle controller. That supplies
workflow tools without the UI's controller-owned lifecycle designation. The
managed-start API uses the `session` key group. Lifecycle ownership tells the
runtime where to send lifecycle notifications; it does not change which API
callers can control or close the session. See
[Agent and tool access](../access-and-security/agent-and-tool-access.md).

Delivery is bounded and can repeat. Pushed invocations and start requests use
bounded retries; failures become observable delivery/start failures and fail
outstanding completion promises. An `accepted` result remains admission-only.
Other outbound notifications do not all have the same retry policy.

Promise resolution checks the pinned producer workflow and universe. The first
terminal resolution wins; duplicate or late replies do not replace it.
`invocation_cancellation` is a best-effort notice that the corresponding
promise is already canceled. Stop the associated work when practical, while
accounting for effects that may already have occurred.

Temporal access and the ability to configure receivers/recipes belong to the
trusted deployment boundary. Envelope identity fields are not cryptographic
proof of the sender. Keep provider secrets and domain I/O in activities, use
the authenticated API for blobs, and keep custom workers off Lightspeed's
internal role queues.

## Verify the integration

Ask the configured agent to call `request_review` for Acorn 1.2. Inspect the
session's tool call, the started workflow on `release-integrations`, the
result blob, and the resumed assistant response. Then test duplicate domain
activity execution, a receiver failure, a deadline, cancellation, and a
missing completion signal with recovery results available.

| Symptom | What to inspect |
| --- | --- |
| Managed creation conflicts | An existing ordinary session or changed immutable declaration. |
| Tool call starts no work | Recipe bytes/fingerprint, workflow registration, namespace, and custom queue pollers. |
| Joined call never resumes | Deadline, reply promise, producer identity, result reference, and holder ID. |
| Started workflow completed but the tool failed | Recovery results missing the required completion key or invalid reply data. |
| Domain work runs twice | Activity retry handling and durable deduplication by invocation identity. |

The [generated workflow contract](../../../crates/temporal-workflow/contract/workflow-contract.md)
and [TypeScript workflow helpers](../../../clients/typescript/src/workflow.ts)
own the exact envelopes, identifiers, and signal/query names. Use those helpers
instead of copying hashing or workflow-ID rules into your integration.
