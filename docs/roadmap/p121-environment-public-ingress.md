# P121: Environment Public Ingress

**Status**

- Proposed 2026-08-11.
- Builds on [P120](p120-incus-environment-provider.md).
- Implements the optional public-application path from
  [P117](p117-environment-compute-plan.md).

## Goal

Let a provisioned environment expose an application through a controlled
hostname without exposing envd, the environment gateway, SSH, Incus, Docker,
arbitrary node ports, or raw Lightspeed RPC endpoints.

The first implementation uses one Cloudflare Tunnel per environment. Public
ingress is independent of the environment control connection and can be
disabled or replaced without changing host protocol or session semantics.

## Policy and ownership

Ingress is allowed only when the provider authorizes it for the environment's
binding and template. Lightspeed does not persist a duplicate
`publicIngressAllowed` binding flag. It records the provider's allocated
hostname and bounded ingress observation on the environment; the provider owns
the allocation policy, while Cloudflare remains authoritative for tunnel, DNS,
and edge TLS state.

The initial environment exposes multiple local services behind one hostname.
Per-service platform APIs, a deployment DSL, and multi-region routing are not
part of this milestone.

Tunnel allocation may begin as a CLI-assisted operator action. Automation is
added only after the allocation, rotation, and deletion lifecycle is proven.

## Credential boundary

The tunnel credential is a service credential for the system-managed
`cloudflared` connector. It must not use the ordinary environment credential
binding mechanism, because those credentials are injected into every
Lightspeed-started process and job.

Install the credential through a protected root/system-service secret path.
The public workload does not receive it. Rotation or ingress revocation updates
the connector without changing envd identity or granting Lightspeed API access.

A tunnel credential is scoped to its own environment tunnel/hostname. Baseline
P120 networking separately blocks sibling binding networks, Incus hosts, and
the trusted control plane. The tunnel itself is not treated as an egress or
lateral-isolation mechanism.

## Lightspeed API access

Control and application credentials remain separate:

- envd uses daemon identity plus incarnation fencing;
- ordinary session processes receive explicitly bound environment credentials;
- a persistent backend that calls Lightspeed receives a dedicated revocable
  universe API key explicitly injected into that backend service; and
- browser/frontend code never receives a Lightspeed bearer key.

The development template provides the stable non-secret Lightspeed API URL and
a small CLI/client helper. ls.bot may add a convenience action for creating and
binding the dedicated backend key, but the underlying create-secret-bind flow
remains authoritative.

## Implementation

- [ ] Add provider-controlled ingress allocation plus Lightspeed observation
      metadata and status.
- [ ] Include `cloudflared` in the development template.
- [ ] Allocate one tunnel and platform-domain hostname per environment.
- [ ] Install the tunnel token through a protected system-service secret path.
- [ ] Configure the connector to reach approved local application routes.
- [ ] Implement rotation, disable, close, and cleanup semantics.
- [ ] Add bounded health/diagnostic views without exposing tokens or local
      service details.
- [ ] Update ls.bot to show the public endpoint and ingress state.
- [ ] Add the optional convenience flow for a dedicated environment backend
      Lightspeed API key.

## Verification

- A binding without ingress permission cannot allocate or claim a hostname.
- Universe A cannot read, mutate, or claim universe B's ingress.
- The public hostname reaches only the configured environment application.
- No node inbound port, envd/gateway endpoint, SSH, Incus API, Docker socket,
  or raw Lightspeed RPC endpoint becomes public.
- Tunnel credentials are absent from arbitrary process/job environments,
  diagnostics, logs, and API responses.
- Credential rotation and environment close revoke the old connector.
- A deployed backend can call only its own Lightspeed universe through its
  dedicated key, while its frontend has no bearer credential.
- External health monitoring observes the public endpoint without gaining
  environment control authority.

## Done

P121 is complete when a session can deploy a backend/frontend from its P120
environment, reach it through the controlled hostname, and the tunnel,
environment-control, and Lightspeed-application credentials remain distinct.

## Deferred

- Node edge ingress with wildcard DNS and host-terminated TLS.
- Per-service deployment APIs or a generic deployment DSL.
- Fine-grained egress policy and hostname policy for untrusted tenants.
- Multi-region ingress, metering, and billing.
