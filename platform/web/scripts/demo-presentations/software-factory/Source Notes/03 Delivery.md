# 3. Delivery

> Source note for the narrative, §3, and deck slides 4–5 (charts: Images/Lead Time per Feature.png, Images/Pull Requests by Origin.png). Lead time is from the `ready-for-build` label to the last pull request open, from Linear and GitHub.

## Lead time per feature
The last four features built by hand against the four through the pipeline. Lead time stops at "pull requests open", because review and merge are still done by people and take as long as they did.

| Feature | Built | Hours to PRs open |
| --- | --- | --- |
| LIN-1388 billing ledger | by hand | 62 |
| LIN-1395 cookie sessions | by hand | 41 |
| LIN-1402 Retry-After header | by hand | 38 |
| LIN-1405 bounded scheduler retries | by hand | 55 |
| LIN-1411 idempotent billing webhooks | pipeline | 7.5 |
| LIN-1414 version on /healthz | pipeline | 4.2 |
| LIN-1418 session refresh hint | pipeline | 2.1 |
| LIN-1421 per-API-key rate limits | pipeline | 1.3 |

The median went from **48 hours to 3.2 hours**. The early pipeline runs were slower because the specs were still in the wiki, which the bots could not read. Moving them into the `specs` workspace fixed that.

## Who opens the pull requests
Merged pull requests a week, by who opened them:

| Week | 29 | 30 | 31 | 32 | 33 | 34 | 35 | 36 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| People | 14 | 12 | 15 | 11 | 13 | 12 | 10 | 12 |
| Pipeline | 0 | 0 | 0 | 2 | 4 | 6 | 7 | 9 |

People did not open fewer pull requests. The pipeline's work is on top, and review is now the bottleneck: the median time from open to merge went from 5 hours to 9.

---
Sources: Linear (label history), GitHub (pull requests merged to main, by author).
