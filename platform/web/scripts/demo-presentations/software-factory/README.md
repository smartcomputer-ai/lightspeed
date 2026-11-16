# Presentations

Decks the team presents, each with the material it is built from. A deck is made in three steps, and only the last one is the presentation:

1. **Source notes** (`Source Notes/`) — one Markdown note per section of the talk. This is where the content is written and corrected. Every number names where it came from: GitHub Actions, Linear, the `specs` workspace, the CHANGELOG in `acme-web`.
2. **Narrative** (`Q3 Engineering Review - Narrative.docx`) — the Word document assembled from the source notes, in order. It is the written version of the talk (it goes out to the team as the pre-read) and the script the deck follows. It is source material for the deck, not the presentation.
3. **Deck** (`Q3 Engineering Review.pptx`) — the PowerPoint presentation. One or two slides per narrative section, built from the Word document; its charts are the images in `Images/`. Each slide's speaker notes name the narrative section and the source note it came from.

A change goes the same way every time: correct the source note, rebuild the narrative, then update the slide. Never edit a number on a slide alone.

## Q3 engineering review — the feature pipeline

Presented by Priya Natarajan at the engineering all-hands.

| File | What it is | Built from |
| --- | --- | --- |
| `Source Notes/01 The Pipeline.md` | What the pipeline is and where people stay in the loop | `runbooks/feature-pipeline.md` |
| `Source Notes/02 One Issue End to End.md` | LIN-1421, from label to three pull requests | `specs/LIN-1421-*.md`; the bots' threads |
| `Source Notes/03 Delivery.md` | Lead time and who opens the pull requests | Linear; GitHub |
| `Source Notes/04 CI and Releases.md` | The slow CI week, the scheduler flake, v2.14.1 | GitHub Actions; `acme-web/CHANGELOG.md` |
| `Source Notes/05 Next Quarter.md` | What changes next, and the asks | the team's retro |
| `Q3 Engineering Review - Narrative.docx` | **Source material:** the narrative, §1–§5 | the five source notes |
| `Q3 Engineering Review.pptx` | **The presentation:** 8 slides for the all-hands | the narrative and `Images/` |
| `Images/Lead Time per Feature.png` | Slide 4 — lead time per feature, by hand and through the pipeline | `Source Notes/03 Delivery.md` |
| `Images/Pull Requests by Origin.png` | Slide 5 — merged pull requests a week, by who opened them | `Source Notes/03 Delivery.md` |
| `Images/CI Job Durations.png` | Slide 6 — CI job medians before, during and after the cache regression | `Source Notes/04 CI and Releases.md` |

## Status

- v2 of the deck, rebuilt after "Why is CI slow this week?" found the dropped npm cache.
- LIN-1421: #491 approved, #492 approved after the CI fix, #493's review is waiting on a provider error — the slide says "in review".
- [ ] Slide 3 still assumes `/api` only for the limit. The open question on the spec is unanswered because the Linear key was rotated; update the slide when Linear is reconnected.
- [ ] Slide 7: v2.14.1 is waiting for the second approval in Octopus Deploy. Change "promoting" to "in production" once it is out.
