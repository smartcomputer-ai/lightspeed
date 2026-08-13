# P121: Provider-Managed Environment Public Ingress

**Status**

- Proposed 2026-08-11.
- Rewritten 2026-08-13 to use direct node-edge ingress rather than a
  per-environment Cloudflare Tunnel.
- Optional and not required to complete P120.
- Builds on [P120](p120-incus-environment-provider.md).

## Goal

Let a provisioned environment expose an HTTP application through a controlled
HTTPS hostname without publicly exposing envd, the environment gateway, SSH,
Incus, Docker, arbitrary VM ports, or raw Lightspeed RPC endpoints.

The first implementation uses a shared ingress proxy at the provider edge:

```text
browser
  -> wildcard DNS and HTTPS
  -> provider-managed edge proxy
  -> environment private address:provider-approved port
```

Public ingress is independent of the environment control and data-plane
connections. Lightspeed still initiates provider and envd traffic on demand;
public application requests never pass through the Lightspeed gateway or
Temporal workers.

## Initial scope

Each admitted provisioned environment may have at most one public hostname.
That hostname forwards HTTP, HTTPS-upgraded WebSocket, and streaming traffic to
one fixed guest port selected by provider template policy. The application may
run its own unprivileged reverse proxy on that port when it needs to serve a
frontend and multiple backend services.

Universe callers do not choose arbitrary destination addresses or ports. Raw
TCP/UDP exposure, multiple platform routes, path-routing configuration, custom
domains, and a deployment DSL are outside the first milestone.

External environments are not included because their networking is not owned
by an environment provider.

## Ownership and durable state

Lightspeed owns the universe's ingress intent and its bounded observation:

- whether ingress is requested;
- the current environment and incarnation;
- the allocated public hostname;
- a small lifecycle status such as `provisioning`, `ready`, `disabled`, or
  `failed`; and
- a revision and timestamps for idempotent reconciliation.

Lightspeed does not store provider network configuration, proxy configuration,
TLS keys, or a duplicate binding-level `publicIngressAllowed` policy flag.

The provider owns authorization and realization. It validates that the
binding and template permit ingress, chooses the hostname and approved guest
port, resolves the current target's private address, and idempotently creates,
updates, observes, or removes the edge route. Repeated requests use stable
request, environment, incarnation, and target identities.

The provider remains stateless. Durable route state lives in the edge proxy's
configuration, while ownership facts remain attached to the Incus target.
After restart, the provider reconstructs the requested route from the call and
backend inventory and reconciles the proxy. It must never adopt or delete a
route whose ownership facts do not match.

Closing an environment disables its ingress before or alongside target
deletion. A changed incarnation cannot inherit a previous incarnation's route
without an explicit successful reconciliation.

## Edge deployment

The initial Incus deployment uses one shared reverse proxy reachable on public
ports 80 and 443. A wildcard DNS record points the platform ingress domain to
that proxy, and the proxy terminates TLS using a wildcard certificate or an
equivalent deployment-managed certificate.

The proxy reaches guest applications only over provider-managed private
networks. Firewall policy allows it to reach the approved ingress port and
does not give it general access to envd, SSH, Incus, Docker, the trusted
control plane, or sibling environment services.

The exact proxy implementation is a deployment choice. Caddy, HAProxy,
Traefik, or a small dedicated ingress service are all acceptable if they
provide:

- an idempotent management interface;
- atomic route replacement;
- hostname-based routing;
- WebSocket and streaming support;
- bounded health observation; and
- auditable route ownership metadata.

The provider-to-proxy management path is a deployment-internal control path.
Any management credential or TLS private key remains in provider/edge
deployment configuration and never enters Lightspeed Postgres or a VM.

## Hostnames and application behavior

Hostnames are allocated under a configured platform domain and must be unique
across all providers using that domain. They should use an opaque stable
provider allocation rather than embed a universe ID or other tenant metadata.

The provider template declares the single guest ingress port. The base image
does not need an ingress agent or platform credential. A workload becomes
reachable by listening on that port; when multiple services are needed, a
VM-local application proxy can route paths to them.

The public response should retain the application's ordinary HTTP behavior.
The edge adds standard forwarded-host/protocol/address headers, applies sane
request and idle limits, and must not inject Lightspeed credentials.

## Lightspeed API access from applications

Public ingress and Lightspeed API access remain separate:

- envd and provider endpoints currently rely on deployment network/transport
  protection;
- ordinary session processes receive explicitly bound environment
  credentials;
- a persistent backend that calls Lightspeed receives a dedicated revocable
  universe API key explicitly injected into that backend service; and
- browser/frontend code never receives a Lightspeed bearer key.

The ingress proxy does not authenticate an application to Lightspeed. The
application remains responsible for its own user-facing authentication.

## Implementation

- [ ] Add a minimal universe API to request, read, and disable ingress for a
      provisioned environment.
- [ ] Persist revisioned ingress intent and bounded observation in Lightspeed.
- [ ] Extend the provider protocol with idempotent ensure, observe, and remove
      ingress operations.
- [ ] Add provider template policy for ingress eligibility and one approved
      guest port.
- [ ] Deploy wildcard DNS, TLS termination, and the shared edge proxy.
- [ ] Implement provider reconciliation of hostname routes to current owned
      Incus targets.
- [ ] Fence routes by universe, binding, environment, incarnation, and target
      ownership.
- [ ] Remove ingress during environment close and recover safely from partial
      enable/disable operations.
- [ ] Expose the public hostname and bounded status without leaking private
      addresses or edge configuration.
- [ ] Update ls.bot to show and manage the optional public endpoint.

The exact proxy product and final API DTO names may be chosen during
implementation; they must preserve the ownership and exposure boundaries
above.

## Verification

- A binding or template without ingress permission cannot allocate a route.
- Universe A cannot read, mutate, route to, or claim universe B's ingress.
- The allocated hostname reaches only the current environment incarnation's
  approved application port.
- Arbitrary ports and private destination addresses cannot be supplied by a
  universe caller.
- envd, the environment gateway, SSH, Incus, Docker, provider control, and raw
  Lightspeed RPC remain unreachable through public ingress.
- A stale incarnation, cloned VM, mismatched target, or reused request cannot
  claim the current hostname.
- Provider, edge-proxy, Lightspeed, and VM restarts converge without duplicate
  hostnames or routes.
- Environment close and ingress disable make the old route unreachable even
  when target cleanup is delayed.
- The proxy supports WebSocket and streaming applications and applies bounded
  connection and request limits.
- No edge management credential, TLS private key, private VM address, or
  Lightspeed API key appears in universe API responses or workload processes.

## Done

P121 is complete when a session can deploy an HTTP backend/frontend into a
P120 environment, request provider-authorized ingress, reach it through the
allocated HTTPS hostname, and reliably revoke that access without exposing a
general VM, provider, envd, or Lightspeed control endpoint.

## Deferred

- Cloudflare Tunnel or another outbound-tunnel implementation for deployments
  that cannot operate inbound edge infrastructure.
- External-environment ingress.
- Multiple platform-managed routes or arbitrary application ports.
- Custom domains and per-host certificate issuance.
- Raw TCP/UDP ingress.
- A generic deployment DSL or application runtime.
- Fine-grained hostname policy for untrusted tenants.
- Multi-region ingress, metering, billing, and denial-of-service protection
  beyond deployment-level limits.
