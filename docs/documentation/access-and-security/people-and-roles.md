# Organizations, members, and roles

Use a shared organization to give a team access to one or more universes. Each
universe belongs to one organization, and a person can belong to several. The
universe switcher shows the active universes you can use, grouped by
organization when there is more than one.

By default, each person gets a **personal** organization when they first sign
in. Its owner is its only human member. When you want people to work together,
create the universe in a shared organization. Platform Admins create shared
organizations under **Admin → Platform → All Organizations**.

## Organization and deployment roles

| Role | What it allows |
| --- | --- |
| **Platform Admin** | Manage the deployment, including all organizations and universes. |
| **Organization Admin** | Manage the organization's members, service users, and settings. Has Owner access to every universe in the organization. |
| **Creator** | Create universes in the organization and become an Owner of each one they create. Also receives the access available to Members. |
| **Member** | Use the universes covered by the organization's default access and any individual universe grants. |

A shared organization's **Default universe access** is the role every human
member receives on every universe in it. The initial setting is **Viewer**.
Set it to **None** to give ordinary members access only through individual
grants or declared access policy. Choose **Owner** only if every member
should be able to administer every universe and read all of its sessions.
Organization Admins can change the default under **Admin → organization →
Settings**.

Roles add together. For example, giving someone Viewer on a universe where
they already have Contributor through the organization leaves their
Contributor access in effect. Service users receive their organization role
and individual universe access, independently of the organization's default
universe access for people.

## Universe roles

Each role includes the permissions of the roles above it in this table.

| Role | What it allows |
| --- | --- |
| **Viewer** | Read visible sessions and browse universe resources. |
| **Contributor** | Start a session from the universe defaults or an existing profile, continue or stop work, decide tool approvals, invoke bots, and edit workspace files. |
| **Operator** | Configure sessions and run options, create and edit profiles and bots, manage resources such as environments and MCP servers, and create workspaces. |
| **Owner** | Manage universe settings and access, read and control all sessions, and delete an unprotected universe. |

Session visibility applies alongside these roles. For example, an Operator can
configure resources and see their own and shared sessions. Owners can also
open other people's private sessions. See [Private and shared
work](private-and-shared-work.md) for the rules on sharing, closing, and
deleting conversations.

A universe can have several Owners. Add a replacement before removing the last
eligible explicit Owner grant. A Creator keeps their Owner grants on existing
universes when they lose the ability to create new ones, as long as they remain
in the organization. Organization and Platform Admins can restore access if
an Owner leaves or is suspended.

## Add people to a shared organization

For a company deployment, people sign in with their [company
account](single-sign-on.md). On a deployment using local accounts, a Platform
Admin creates users under **Admin → Platform → Users**.

1. As an Organization Admin, open **Admin**, select the organization, and go
   to **Members → Add person**. Enter the person's **User ID** from their
   account **Settings**. Platform Admins can find users by name or email.
2. Choose **Member**, **Creator**, or **Admin**. **Member** is the default.
3. To give the person a role in one universe, open that universe's **Settings
   → Members → Add member** and choose the role. A universe Owner can do this
   for an existing organization member or service user.

For a team managed through configuration, an organization document can set
membership by identity or company group. A universe document can declare
additional access. See [Configure organizations and
access](../deployment/configuration-as-code.md#configure-organizations-and-access).

## Remove or change access

In the universe's **Settings → Members**, open a grant to change its role or
remove it. The person may still have access through the organization default,
a declared policy, or an administrative role.

Remove a person from **Admin → organization → Members** to remove their
manual organization membership. If this ends their membership, it also removes
their explicit grants in the organization's universes. If a membership policy
still includes them, their grants stay in effect. When membership ends through
a policy change, existing universe grants become inactive and can apply again
if the person returns.

Changes apply to subsequent requests. To end work already running, cancel the
run or close the session separately. A Platform Admin can suspend access
across the deployment under **Admin → Platform → Users**.

## Use the right settings page

- **Account menu → Settings** holds your profile, sign-in settings, and
  [personal API keys](api-keys-and-service-access.md).
- **Universe → Settings → General** holds the universe's name and handle,
  features, and lifecycle settings. See [Managing
  universes](../deployment/multi-tenancy.md) for protection, expiry, and deletion.
- **Admin → organization → Settings** holds policies for a shared organization,
  including default universe access and deletion protection.
- **Admin → Platform → Settings** holds deployment-wide defaults, including
  personal organization provisioning and universe limits. Platform Admins can
  give an organization its own limit from its settings.
