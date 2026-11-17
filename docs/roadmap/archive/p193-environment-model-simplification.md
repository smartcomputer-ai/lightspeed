# P193 — Environment model simplification

**Status:** Implemented in the working tree on `env-protocol`, 2026-10-27,
not committed. Workspace `cargo check` and CI Clippy pass; unit, integration
and live suites have not been run yet. Greenfield: one protocol bump (envd
and controller protocol version 4), the store baseline rewritten in place
(schema revision 11), contracts and TypeScript consumers regenerated. Every
existing database needs `./dev.sh reset` and a fresh Temporal volume.

Lightspeed becomes the inventory and router for environments, and whoever
runs a machine owns what happens while it runs. Lightspeed keeps durable
retries only for the two things it alone decides: **create until accepted**
(and opened by one data-plane handshake) and **close until gone**. Power,
idle, wake, ingress, placement, pools and snapshots belong to the provider
for provisioned environments and to the operator's own infrastructure for
registered and external ones.

This reverses the power and idle policy of
[P126](archive/p126-environment-power-and-idle-policy.md) and the
provider-is-policy-free rule of
[P117](archive/p117-environment-compute-plan.md).

## Decisions

1. **Providers own power policy.** The power intent, the power reaper,
   wake-on-use writes and power and ingress convergence are deleted.
   `environments/power/put` and `environments/ingress/put` are synchronous
   pass-throughs that store only the returned observation; a change made
   outside Lightspeed is never reverted.
2. **Idle is a template default plus a create-time override.** The optional
   `idle` on `target/create` is passed through to the provider; there is no
   per-environment idle edit after create and no `target/configure`.
3. **Sandbox per session: dropped for now.** A `provision {providerId,
   templateId, idle?}` session attachment, resolved at session start or
   profile apply into an environment that closes with its session, was
   implemented and then removed before commit at the owner's request. The
   attachment grammar stays `environmentId | inherit`
   ([P173](p173-session-config-attachments.md)); there is no
   `closes_with_session_id` column, session-close hook, backstop sweep,
   `sessionClosed` close reason, reserved `prov:` request-id prefix, or purge
   of closed provisioned rows. A caller that wants a machine per session
   creates one with `environments/create`, attaches it, and closes it itself.
4. **Leftover process groups pin idle.** envd's `env/getIdle` holds
   `idleForMs` at zero while a leftover process group runs, as it does for
   running processes and jobs.
5. **The external kind stays.**
6. **A bounded wait for a disconnected registered environment:** two minutes
   for persistent keys, none for ephemeral keys.
7. **Enrollment is deferred** (provisioned machines that register back to
   the gateway; see [P119](archive/p119-environment-daemon-gateway-enrollment.md)).
8. **Selector attachments are deferred.** Fleets use explicit ids plus
   `environments/update` labels.
9. **Key rotation through a child secrets table**, so the group, the
   `registrationKeys` allowlists and the identity mode survive a rotation.
10. **Closed registered rows are retained**: a closed identity stays spent.
    Per-event work goes to the provisioned kind.

Also decided:

- The provider credential is optional on every endpoint scheme. Without one,
  no `Authorization` header is sent on control or data dials, and the Incus
  `credentialFile` is optional.
- The data dial is the wake signal. A provider holds a dial to a
  powered-down target while it wakes it, then upgrades or answers a typed
  refusal (`503 {waking|booting}` with `Retry-After`, `410 {gone}`,
  `409 {failed}`), which the gateway forwards. `target/setPower` is an
  optional synchronous pass-through for manual buttons. Environment source
  discovery dials with a no-wake marker, which the gateway honours by asking
  the provider for the target's current power before it dials.

## Model

- One `environments` row with three kinds (provisioned, external,
  registered) and a five-value lifecycle: `provisioning`, `open`, `closing`,
  `closed`, `failed`, with `state_message` and `close_reason`. Provider
  observations are display facts; the display status is derived.
- One use gate (Use, Wait or Gone) used by selection, the resolver, job
  activities, discovery and the worker route. Nothing that only reads a row
  writes to it.
- Incarnations, binding ids and revisions, adopt, cached controller
  features and the provider-facts refresh loop are deleted. Bindings are
  `(universe_id, provider_id, enabled)`; the data route base is optional
  operator configuration, derived from the controller endpoint by default.
- Registered liveness is written on transitions and as a coarse stamp at
  most every five minutes. The gateway marks every registered row
  disconnected at startup (one environment-gateway replica), runs a 30 s
  registered sweep, and notifies control connections of a close in process.
- New API methods: `environments/update`,
  `environments/registration-keys/update` and `.../rotate`. Removed:
  `environments/idle-policy/put` and `deployment/environments/adopt`.

## Open

- Run the unit, integration and live suites, including the rewritten
  provider, registration, profile and sub-agent live suites.
- Universe deletion with registered environments or environment credentials
  may be refused by `ON DELETE RESTRICT` foreign keys, depending on cascade
  order (predates this work).
- The gateway and the environment client each carry the status fallback for
  a refused upgrade whose body was not read; one shared decoder in the
  protocol crate would keep them from drifting.
- A third-party provider must adopt the slimmed `TargetOwner`, the typed
  data-route refusals and the idle obligations in lockstep with protocol
  version 4.
- Deferred: enrollment, selector attachments, `target/configure`, a
  multi-replica environment gateway, per-session provisioned environments
  (decision 3).
