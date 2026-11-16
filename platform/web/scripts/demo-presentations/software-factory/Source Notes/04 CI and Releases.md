# 4. CI and releases

> Source note for the narrative, §4, and deck slides 6–7 (chart: Images/CI Job Durations.png). Timings are medians of ci.yml runs on main, from GitHub Actions.

## The slow week
CI felt slow all week. The status board was down (502), so the timings came from the workflow runs instead. Every job was flat except `install`:

| Job | Last week | Slow week | After #486 |
| --- | --- | --- | --- |
| install | 41 s | 2 m 58 s | 44 s |
| lint | 48 s | 51 s | 49 s |
| test (node 22) | 3 m 10 s | 3 m 22 s | 3 m 08 s |
| build image | 2 m 05 s | 2 m 11 s | 2 m 06 s |

**Cause:** #483 bumped `actions/setup-node` to v5 and, in the same edit, dropped `cache: npm`, so every run did a cold `npm ci`. **Fix:** #486 restored the cache, taking about 2 minutes off every run.

## The scheduler flake
The scheduler retry test failed about one run in five, because it depended on the wall clock. #487 makes it use fake timers. The audit that followed found no other wall-clock dependencies in `src/scheduler`.

## v2.14.1
Both fixes ship in v2.14.1. The deploy checklist passed: staging soaked for four hours, with no new Sentry issues and a flat dead-letter count. Promotion to production is waiting for the second approval in Octopus Deploy, as the runbook asks.

---
Sources: GitHub Actions (ci.yml runs on main); acme-web/CHANGELOG.md; runbooks/deploy-checklist.md.
