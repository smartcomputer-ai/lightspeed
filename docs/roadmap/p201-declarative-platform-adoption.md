# P201 — Declarative Platform adoption and access

**Status:** Implemented and locally validated, 2026-11-13; pending PR checks. Revised to follow the
updated Kubernetes deployment request: federation is deferred.

## Goal and scope

Complete [declarative universe configuration](p199-declarative-universe-configuration.md)
so a universe Helm release needs no Platform UI step. Keep the existing deployment
API key and generated registration keys. In this delivery:

1. Runtime-owned display names and automatic Platform adoption of named universes.
2. Members and directory-group role mappings in universe documents.
3. Bounded startup retries with `config apply --wait`.

API-key federation and registration-key federation are later, optional work.
There are no credential-table changes in this delivery.

## Universe configuration

Add `deployment/universes/configuration/put`, authorized by the existing
`deployment/universes` group. It updates optional `displayName`, `members` and
`directoryGroupMappings` fields. Omitted fields retain their previous values;
empty lists clear the corresponding declared grants. A display name is a
nonempty string of at most 120 characters. Runtime persists this desired
configuration and includes it in universe read/list results. Runtime continues
to authorize its own API exclusively through existing API-key scope and groups.

Universe documents expose the same three fields at their top level. Apply/diff
compare only fields present in the document. Export preserves declarations,
including explicitly empty lists. Names and access declarations are written
before other resources. Existing documents remain valid.

```yaml
schemaVersion: 1
kind: universe
handle: research
displayName: Research
members:
  - email: owner@example.com
    role: admin
  - issuer: https://login.example.com/
    subject: stable-directory-subject
    role: operator
directoryGroupMappings:
  - issuer: https://login.example.com/
    group: research-readers
    role: viewer
```

A member selects either a verified email or an exact issuer/subject pair.
Roles are `viewer`, `contributor`, `operator`, or `admin`. Directory groups are
opaque exact values, qualified by issuer. No role permits platform administration.
Duplicate selectors are rejected. Missing users are not created; access takes
effect when the corresponding identity signs in.

## Platform reconciliation and authority

Platform periodically reads the configured Runtime inventory and adopts named
universes without creating a fictitious owner. Existing platform administrators
retain their normal access. It synchronizes declared names and access policy in
a transaction, with uniqueness/conflict checks and serialized concurrent syncs.
Unnamed universes retain manual adoption. A Runtime outage preserves the last
successful projection; errors remain visible in service logs and subsequent
passes retry. Archived universes stay archived.

Manual memberships remain independent. The effective universe role is the
highest role supplied by a manual membership, a declared member, or a matching
directory group. Removing a declaration removes only its grant; it never deletes
manual membership. Policy is checked from the Platform database on requests.
Directory groups come only from verified company identity claims and refresh on
sign-in; existing session maximum age bounds provider-side revocation delay.
An absent or malformed group claim clears previous group grants. Email selectors
require a verified email; exact issuer/subject selectors do not depend on email.

The Runtime stores desired declarations, not user accounts or effective web
memberships. Platform remains the enforcement boundary and never grants Runtime
API authority based on these declarations. UI names under Runtime management
cannot be edited locally. Declared access remains visible as configuration, not
misrepresented as manually editable membership rows.

## Waiting for startup

`config apply --wait [SECONDS]` defaults to 60 seconds when present. Retry only
connection failures/timeouts and transient HTTP gateway/service failures during
connection discovery, with a bounded total deadline. Refuse credentials,
configuration, and protocol errors immediately. Do not replay mutations whose
outcome is unknown. Without the flag, preserve immediate failure behavior.

## Storage and compatibility

A Runtime migration adds nullable `display_name` and a JSON object containing
access declarations to `universes`. Platform stores the last projected name/access
configuration with its universe row, and verified directory group claims with the
company user. Existing rows default to unmanaged names and empty declared access.
No API keys, registration secrets, tokens or signing keys are changed or copied.
Regenerate API and configuration schemas and clients, and align release revisions.

## Validation and progress

- Updated design recorded before implementation.
- Removed the unfinished federation implementation after reprioritization.
- Windows snapshot run 43441133725: compilation and packaged smoke test passed;
  provenance attestation failed on a network timeout to Sigstore Rekor. No evidence
  of a Windows code defect; no unrelated code fix planned.
- Implemented Runtime configuration storage/API, configuration apply/diff/export,
  automatic Platform adoption, declared access enforcement and read-only UI, and
  bounded CLI startup retries. Runtime schema revision 22; Platform revision 5.
- Regenerated Rust API contracts, configuration schema, TypeScript and Python
  clients, and Platform consumers.
- Passed scoped API/CLI/store tests, all 642 Runtime unit tests, isolated PostgreSQL
  configuration tests, Platform backend and affected frontend tests, real OIDC
  login/access-revocation coverage, and configuration export/reapply coverage.
- Passed workspace Clippy with all targets and warnings denied, TypeScript
  typechecking and production builds, migration upgrade checks, release metadata
  checks, SDK tests, and Python generation/typechecking/tests.
- No full deployed browser walkthrough was run. PR checks and auto-merge are the
  remaining delivery steps.
