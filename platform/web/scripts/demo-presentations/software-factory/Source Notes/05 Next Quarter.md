# 5. Next quarter

> Source note for the narrative, §5, and deck slide 8. From the team's retro; the asks are Priya's.

## What changes
- **Review is the bottleneck.** Pull requests from the pipeline wait 9 hours for a person (median). Each squad gets a daily review slot, and pr-reviewer's approval goes first so people start from its notes.
- **Answer the spec's questions before building.** LIN-1421 was built on an assumption, because the open question could not reach the issue. Intake will hold the build when a spec has an open question that changes the interface.
- **Credentials rotate without breaking the pipeline.** The Linear key rotation silently stopped intake from commenting. Rotations go on the deploy checklist, with the MCP servers page as the step that reconnects.

## Follow-ups from LIN-1421
- Delete the old limiter, `src/api/rate-limit.ts` (#472), now that the bucket has landed.
- Per-key overrides in the admin UI. This was a non-goal for LIN-1421; the configuration shape already allows it.

## Asks
1. One reviewer from each squad on the daily slot, starting week 38.
2. Someone with Linear admin to reconnect the workspace key this week.
3. Agreement that "PRs open" is the lead-time number we track, with review time beside it.
