# 2. One issue, end to end

> Source note for the narrative, §2, and deck slide 3. Taken from /specs/LIN-1421-rate-limits.md, /specs/LIN-1421-plan.md and the bots' threads; times are minutes after the label went on.

## LIN-1421: per-API-key rate limits
Marco asked for it: one noisy integration should not starve the others in the same org. Over the limit, `/api` answers `429` with `Retry-After`.

## What happened
- **Minute 0:** the `ready-for-build` label fires the Linear webhook.
- **Minute 2:** intake writes the spec. It found the unmounted limiter from #472 and the refill bug that #482 shipped and reverted, and it wrote both into the spec's context.
- **Minute 7:** planner splits the work into three independent tasks: a, the token bucket; b, the middleware and configuration; c, the metrics and customer docs.
- **Minutes 8–80:** implementer builds each task in its own sandbox, with repo-explorer and test-writer as sub-agents, and opens **#491**, **#492** and **#493**.
- **Along the way:** a lint failure on #492's branch is steered into the running task by ci-watch, and fixed in the same pull request.
- **Review:** #491 and #492 are approved. #493's review hit a provider error (529, overloaded) and is waiting to be replayed.
- **Notes:** release-scribe adds the 429 entry to Unreleased in CHANGELOG.md.

## What it could not do alone
The spec has one open question: does the limit apply to `/api` only, or to every authenticated route? Intake could not post it to the issue, because Linear returned 401 after the workspace key was rotated. **The assumption is `/api` only** until someone answers.

---
Sources: specs workspace; the intake, planner, implementer, ci-watch, pr-reviewer and release-scribe threads.
