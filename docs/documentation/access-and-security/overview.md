# Access and security

Lightspeed gives a team a universe in which to run agents and keep their work.
The Platform decides which people belong to that universe and what they may
do. The runtime authenticates trusted components with configured tokens, partitions
data by the requested universe, and records who asked for the work. Those responsibilities meet at the API
boundary, so the runtime can also serve applications that supply their own
sign-in and access rules.

Suppose Acorn uses Lightspeed to investigate incidents. An investigator can
start a private session, ask an agent to examine a problem, and share the
conversation when it is useful to the team. Acorn's administrators can read
private sessions too. Meanwhile, a bot can keep handling incoming incidents
without depending on the investigator staying signed in.

## The pieces of the access model

Each part answers a different question:

| Part | What it establishes |
| --- | --- |
| Universe | Which team's resources a request addresses. Each universe belongs to one Platform organization; an organization can have many universes. |
| Organization and membership | A personal or shared group of universes, with Admin, Creator, or Member roles and separate universe grants. Backed by Better Auth organizations. |
| Role | Which operations a person may request: Viewer, Contributor, Operator, or Owner. |
| Runtime token | Full deployment authority for a trusted component or operator. |
| Actor | Who a trusted client says requested an operation. The Platform supplies the authenticated user or service user's ID. |
| Session visibility | Whether a session is private to its creator and universe Owners, or shared with the universe through the Platform. |

An actor is attribution, not a role or an execution identity. Core stores the
identifier without looking up a person. It can compare that identifier when
filtering session lists, but the Platform decides which filter a person must
use and whether they may act on a particular session.

## Follow a request

When the investigator starts a run, the browser sends the request to the
Platform. The Platform reads their universe membership, checks that their role
allows starting runs, and checks that they can access the session. It then
calls the runtime with its configured token, the universe UUID, and the person's
actor ID.

Runtime tokens are infrastructure credentials. Every token can select any
universe, call every method, and name any actor. The Platform alone applies
caller permissions. A standalone runtime must have a trusted gateway or run
on a trusted network. Direct access can read private sessions too.
[Runtime tokens and service access](api-keys-and-service-access.md) describes
configuration and rotation.

## People requesting work and agents carrying it out

Sessions execute using their configured tools and resources within the
universe. A run does not inherit the requesting person's role, and the runtime
does not recheck that person's membership before each model call. Removing an
investigator from Acorn blocks their next Platform request; it does not stop
an investigation they already started or a bot they created.

Internal bot and delegation controllers have a separate, narrow set of rules
for the sessions they manage. External tools use their configured credentials.
Configurator setup uses a universe-scoped service-user key with a reviewed
configuration operation allowlist. [Agent and tool access](agent-and-tool-access.md) follows both paths
and explains what each check protects.

## What is recorded today

Session and bot records identify their creator. Session events identify who
requested a run, steering, or cancellation, and who decided a tool approval.
An attribution can name an asserted actor, the configured token name, a local
caller, or the runtime controller responsible for internal work. Changing a
token secret under the same name does not change its attribution.

The Platform also keeps a durable access history for company admission and
admin-status changes, emergency sign-ins, suspension, session revocation,
local membership edits and successful permanent session
deletion through Platform routes. Platform admins can view the latest 100 records
under **Admin → Audit log**. Records contain identifiers, action, time,
outcome and safe details; permanent deletion includes the actor, universe, target
session and all removed session IDs. Records survive deletion of users and
sessions. Provider tokens and passwords are never included.

For sessions, only permanent deletion is audited: creation, sharing, closing,
force-closing, retention changes and soft deletion are not. Automatic retention
work appears in runtime operational logs, not this audit log. Core operations
made directly with a runtime token do not pass through this trail. Comprehensive
gateway-operation auditing, audit retention controls and export are deferred.
Core session attribution remains subject to session retention and deletion.
Company OIDC sign-in is
supported; directory synchronization, SCIM and member invitations are deferred.

## Choose the next detail

- [People and roles](people-and-roles.md): sign in, add members, assign roles,
  and understand the effect of removing access.
- [Company sign-in (SSO)](single-sign-on.md): configure your identity provider,
  emergency access, and session renewal and revocation.
- [Private and shared work](private-and-shared-work.md): investigate privately,
  share a session, and understand access to bots and delegated work.
- [Runtime tokens and service access](api-keys-and-service-access.md): configure
  gateway authentication and connect clients and services.
- [Agent and tool access](agent-and-tool-access.md): understand tools,
  controller authority, credentials, approvals, and stopping work.
- [Tenant isolation and data protection](tenant-isolation-and-data-protection.md):
  evaluate the storage, infrastructure, and compute boundaries of a universe.
