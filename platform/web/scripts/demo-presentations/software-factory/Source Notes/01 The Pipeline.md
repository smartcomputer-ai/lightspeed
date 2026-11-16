# 1. The pipeline

> Source note for the narrative, §1, and deck slides 1–2. The stages are the ones in runbooks/feature-pipeline.md; correct them there first.

## What it is
Since week 32, a Linear issue labelled `ready-for-build` becomes pull requests without anyone writing code until review. Six bots hand the work along with events, and each one keeps its own thread:

| Stage | Bot | Hands on |
| --- | --- | --- |
| Spec | intake | `feature.spec` with `/specs/<ISSUE>-<slug>.md` |
| Plan | planner | one `task.ready` per task, from `/specs/<ISSUE>-plan.md` |
| Build | implementer | one sandbox and one pull request per task; `pr.opened` |
| CI | ci-watch | a failure on a task branch, steered into the running task |
| Review | pr-reviewer | `review.approved`, and `release.note` for customer-visible changes |
| Notes | release-scribe | the Unreleased section of CHANGELOG.md |

## Where people stay in the loop
- **People merge.** Nothing in the pipeline pushes to main.
- **People approve production.** Octopus Deploy waits for a second person, who has read the notes and can roll back.
- **People own the spec.** The spec is in the `specs` workspace; editing it and replaying `task.ready` rebuilds a task.

**One line for the title slide:** One issue in, reviewed pull requests out.
