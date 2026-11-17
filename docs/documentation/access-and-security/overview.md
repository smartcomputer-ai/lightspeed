# How access works

Sign in to the Platform to work in a universe. A **universe** holds the
sessions, bots, workspaces, and other resources a team uses. An
**organization** groups universes and manages access for people and service
users. A deployment can host several organizations, including personal
organizations for individual work.

Suppose the Acorn organization has separate universes for release work and
incident response. Its members can see both as Viewers by default. A release
engineer with Contributor access in the release universe can run an agent
there, keep the conversation private while investigating, and share it with
the rest of the universe when it is ready. An Acorn Organization Admin can
read it at any stage.

## Who can request work

The Platform checks the caller's permissions on every request to the web app,
CLI, or API. What a caller can do depends on their organization and universe
roles, the resource they want to reach, and any restrictions on their API key.
A service user is an identity for automation with its own roles and keys.

Once admitted, a run uses the tools and credentials configured for the
session. If someone leaves Acorn, their next request requires current access.
Acorn can separately stop their existing work.

The Platform forwards admitted requests to the runtime with a universe and an
**actor**, the ID of the person or service user requesting the operation. The
actor is recorded with the work. A direct runtime token has deployment-wide
authority and belongs only to trusted components and operators. Direct runtime
requests use the universe to select the resource partition.

## What is recorded today

Sessions and bots record their creator. Session events record who requested a
run, steered or cancelled it, or decided a tool approval. Requests through the
Platform identify the authenticated actor. For a change requested directly,
the runtime attributes it to the asserted actor or the configured token name.

Platform Admins can review sign-in and access changes under **Admin →
Platform → Audit log**. The log includes company admission changes, account
suspension and session revocation, administrative changes to memberships,
universes and API keys, and permanent session deletion through the Platform.
It records identifiers, times, outcomes, and selected details. For everyday
run activity, open the session itself.

## Find the right guide

- [Organizations, members, and roles](people-and-roles.md) explains how to
  give people access and what each role can do.
- [Company sign-in (SSO)](single-sign-on.md) covers connecting an identity
  provider and managing company access.
- [Private and shared work](private-and-shared-work.md) explains who can see
  a conversation and what happens when you share it.
- [API keys and runtime tokens](api-keys-and-service-access.md) covers
  connecting scripts, integrations, and trusted components.
- [Agent and tool access](agent-and-tool-access.md) describes the permissions
  a session uses to carry out work.
- [Tenant isolation and data protection](tenant-isolation-and-data-protection.md)
  describes where the universe boundary applies across storage and compute.
