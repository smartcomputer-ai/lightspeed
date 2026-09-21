# Later — Typed decision models alongside agent runs

**Status:** Exploratory, 2026-09-21. No implementation committed. Evaluate
bounded uses of TypeSafe AI's Jev before introducing a general decision-model
abstraction or changing the agent loop.

## Motivation

Jev accepts context and predefined questions, returning choices, scores, and
probabilities. It does not generate replies, code, or explanations. Its
potential role in Lightspeed is to make narrow semantic judgments around
generative agents: which skill seems relevant, whether an event warrants
investigation, or whether a completed run needs review. See TypeSafe's
[model overview](https://docs.typesafe.ai/concepts/system-one).

The opportunity is to combine deterministic workflows, inexpensive judgments,
and generative runs at different points in a task. Durable execution,
permissions, context, and audit history remain useful across those choices.
Simple stateless classification may need little infrastructure; Lightspeed's
value is coordinating decisions and actions over time. A decision call should
not require creating a conversational session solely to host it.

The published Jev price at the research date is $0.042 per million input
tokens, with free outputs. One million calls of 1,000 total input tokens would
therefore cost about $42 in model fees, excluding retries and infrastructure.
This makes scheduling, storage, context assembly, and downstream agent costs
worth measuring together. Prices, model limits, and early-access capacity
must be rechecked before implementation. See the
[model specifications](https://docs.typesafe.ai/models).

## Candidate pilots

| Candidate | Proposed behavior | Evaluation priority |
| --- | --- | --- |
| Skill suggestions | Rank available skills and add an optional suggestion for the current task. | First integration: bounded and easy to compare with existing selection. |
| Bot event triage | Assess whether an admitted event needs a generative run, its category, or its urgency. | Start in shadow mode; potentially larger savings from avoided runs. |
| Run review | Flag completed runs that may need human inspection or another reasoning pass. | Later experiment once useful labels and review outcomes exist. |
| Specialist selection | Suggest among already permitted profiles or handlers. | Later; compare against existing rules and agent delegation. |

### Skill suggestions

Lightspeed already publishes a skill metadata catalog and lets the model read
the relevant skill through ordinary file tools. A pilot could rank that
catalog, inspect a shortlist, and supply a nonauthoritative suggestion. It
must be able to suggest no skill and preserve explicit user selection.

Keep the full catalog, its ordering, and existing discovery boundaries.
Publish any changing hint through normal context admission at a safe turn
boundary; do not reorder the cached prefix on every turn. Tie the suggestion
to the task and catalog revision so it cannot silently apply to later work.
See [context and storage](../../documentation/how-it-works/context-and-storage.md).

TypeSafe's [skill suggestion cookbook](https://docs.typesafe.ai/cookbooks/skill_suggestion)
describes a similar ranking-and-verification pattern. Its reported gains are
vendor results on another harness, not evidence of a Lightspeed improvement.

### Bot event triage

Current trigger filtering, routing, coalescing, and busy-session delivery use
explicit policy, including CEL expressions. These operations are already
cheap. The proposed benefit is identifying semantic cases that those rules
cannot capture, then avoiding unnecessary downstream runs. See
[bot routing](../../documentation/using-lightspeed/bots-and-triggers.md#choose-where-and-when-events-run).

Preserve the existing gate in
[bots beyond federation](pNNN-bots-beyond-federation.md): pursue triage only
when deterministic filters demonstrably fall short. Initially record what
the model would recommend while continuing normal event delivery. Do not
suppress events during the shadow evaluation. Any later learned suppression
needs a durable, inspectable decision record; existing CEL refusals are not
stored as ordinary bot activity.

## Integration boundaries

- Perform inference in a runtime adapter or Temporal activity, outside the
  engine and domain policy crates. Keep Jev's request and response shapes
  native to the provider. The current conversational provider contract is
  not a drop-in fit for these evaluations.
- Persist input and question/schema references, the resolved model version,
  native response reference, policy version, and applied outcome. Keep large
  payloads in CAS and retain their references appropriately. Deterministic
  state needs only the neutral facts required to branch. Replay consumes the
  recorded result without calling the provider again.
- Correlate decisions with a stable operation identity and input revision.
  Activity retries must not apply an outcome twice. Specify timeout,
  cancellation, and unavailable-provider behavior; for the pilots, fall back
  to normal skill selection or existing event delivery.
- Let code enforce permissions, routing ownership, budgets, and valid
  destinations. A model may recommend among authorized choices; its score
  never grants authority or replaces exact policy checks.
- A model-invoked helper can use the existing
  [workflow-tool protocol](../../documentation/how-it-works/tools-and-controller-workflows.md#bind-workflow-tools-through-trusted-declarations).
  Automatic pre-run triage requires controller orchestration in addition to
  such a tool. Preserve workflow starts/signals across subsystem boundaries.
- Batch independent questions against the same relevant context where useful.
  Measure whether one activity can hold the batch rather than scheduling a
  separate durable operation for every scalar answer. Generalize the adapter
  only after a pilot demonstrates a repeated need.

These choices follow the existing
[effect and replay boundaries](../../documentation/how-it-works/architecture.md).

## Evaluation and limits

Guaranteed output types do not establish decision correctness. TypeSafe
documents failures involving adversarial input, irrelevant context, numerical
precision, and multi-step reasoning. Keep arithmetic and exact checks in
code; use compact relevant inputs. See
[Jev's known limitations](https://docs.typesafe.ai/model-jaggedness/jev-1.13).

The `confidence` field summarizes a probability distribution; a value of
0.9 is not automatically 90% correctness on a Lightspeed workload. Fit and
validate thresholds for each task and model version, with an explicit
uncertain/fallback path. See [confidence semantics](https://docs.typesafe.ai/confidence).
TypeSafe's headline workflow comparisons use model-consensus reference
answers, so they do not establish production accuracy or end-to-end speedups
for this system. See the [evaluation methodology](https://evals.typesafe.ai/).

Before promoting a pilot, compare against existing behavior on representative,
held-out examples, including ambiguous requests, no-match cases, and hostile
input. Measure:

- Skill selection accuracy, unnecessary skill reads, and missed relevant skills.
- Missed actionable events, unnecessary activations, and review/fallback rate.
- Actual error rates across score thresholds and changes between model versions.
- Total model and runtime cost, downstream work avoided, and p50/p95 latency.
- Rate-limit and outage behavior, retry handling, replay, and decision visibility.

Choose acceptance thresholds before enabling automatic action. Pin the model
version used for evaluation and compare upgrades before adopting them. An
optional skill hint should justify its added latency and cost; learned event
suppression needs an acceptable missed-event rate and inspectable outcomes.

## Next decision

Start with an optional skill-suggestion experiment and a separate shadow
evaluation of bot events. Revisit implementation only with measured evidence
that either improves quality or total cost. The remaining design choices are
where decision records live, how per-tenant budgets and provider capacity are
enforced, and which configuration surface is justified by the successful pilot.
