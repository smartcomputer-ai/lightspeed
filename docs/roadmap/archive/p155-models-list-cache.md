# P155 — `models/list` Discovery Cache

**Status**

- Implemented 2026-09-03. Moved out of [P153](p153-session-metadata.md), where
  it was recorded as an aside during the first Harbor evaluation
  ([P149](p149-harbor-end-to-end-agent-evaluation.md)).

## Problem

`models/list` queries every provider live on each call. An evaluation job
that starts 12 trials at once issues 12 provider catalog requests within
seconds, and the first hosted trial hit a transient provider 500 that way.

## Decision

- Each universe's gateway service owns an in-memory, per-provider cache. Its
  10-second TTL collapses a burst to one upstream request per gateway process
  without maintaining a durable catalog. This is a real TTL: it counts from
  insertion, unlike session retention ([P154](p154-session-retention.md)).
- Failures are cached for 2 seconds, so an outage is retried but not hammered.
- Fetches are single-flight per provider. Different providers retain
  independent locks and are still queried concurrently.
- Successful provider creation and deletion invalidate that provider's local
  entry. Provider configuration and credential changes are delete-and-create
  operations, so API key rotation and changes to endpoint URLs, headers, OAuth
  bindings, or admitted API kinds do not reuse the old local result. A cache
  generation prevents an already-running request from repopulating an entry
  after invalidation.
- Deployment credentials and endpoints are captured when the gateway clients
  are constructed; changing them requires a process restart, which also drops
  this cache.
- The cache is deliberately not distributed. Other gateway processes observe
  a provider change when their entry reaches the 10-second TTL; no database
  invalidation or pub/sub mechanism is justified for this burst optimization.

## Acceptance

- Twelve concurrent `models/list` calls in one universe and gateway process
  produce one upstream request per provider.
- A provider failure is returned to callers inside the negative TTL and
  retried after it.
- Two universes never share an entry.
- Replacing a provider credential or transport configuration does not reuse
  the previous local success or failure.
- A provider result already in flight when invalidation occurs is returned to
  its original caller but is not inserted into the new cache generation.
