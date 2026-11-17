import { useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { AccountRole, PlatformOperationResult } from "@lightspeed-ai/platform-shared";
import { platformApi, useAccounts } from "@/lib/platform-api";
import { usePlatformAdmin } from "@/lib/permissions";
import { AddIcon, BackIcon, EditIcon } from "@/icons";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import {
  CreateAccountDialog,
  MemberDialog,
  ApplicationDialog,
  PolicyDialog,
  ConfirmDialog,
} from "@/components/admin/account-dialogs";
import {
  Table,
  TableBody,
  TableCard,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { LoadingNote, PageHeader, SectionHeader } from "@/components/page";

type Account = PlatformOperationResult<"platform/accounts/read">["account"];
type Member = PlatformOperationResult<"platform/accounts/members/list">["members"][number];
type Application = PlatformOperationResult<"platform/accounts/applications/list">["applications"][number];
const roleLabels: Record<AccountRole, string> = {
  member: "Member",
  creator: "Creator",
  admin: "Admin",
};
const kindLabel = (account: Account) =>
  account.kind === "personal" ? "Personal" : account.kind === "development" ? "Development" : "Shared";

export function AccountsPage() {
  const client = useQueryClient();
  const accounts = useAccounts();
  const admin = usePlatformAdmin();
  const [searchParams, setSearchParams] = useSearchParams();
  const selected = searchParams.get("account");
  const [createOpen, setCreateOpen] = useState(false);
  const manageable = accounts.data?.accounts.filter((account) => admin || account.role === "admin") ?? [];
  const account = manageable.find((account) => account.id === selected);
  const canCreate = accounts.data?.canCreate ?? false;
  const refresh = () => client.invalidateQueries({ queryKey: ["accounts"] });
  return (
    <>
      {selected && (
        <Button nativeButton={false} className="mb-4" variant="ghost" render={<Link to="/admin/accounts" />}>
          <BackIcon data-icon="inline-start" />
          Accounts
        </Button>
      )}
      <PageHeader
        title={account?.name ?? "Accounts"}
        description={
          account
            ? `${kindLabel(account)} account · ${account.enabled ? "Active" : "Suspended"}. Manage account membership, applications, and policy.`
            : "Accounts you can administer. Open an account to manage its people, applications, and policy."
        }
        actions={
          !selected && canCreate ? (
            <Button onClick={() => setCreateOpen(true)}>
              <AddIcon data-icon="inline-start" />
              New account
            </Button>
          ) : undefined
        }
      />
      {accounts.error && (
        <p role="alert" className="text-destructive">
          {accounts.error.message}
        </p>
      )}
      {accounts.isLoading && <LoadingNote />}
      {selected && accounts.data && !account && (
        <p role="alert">
          This account is unavailable or you do not have permission to administer it. Return to Accounts to
          choose another.
        </p>
      )}
      {!selected && accounts.data && !manageable.length && (
        <p className="text-sm text-muted-foreground">
          You do not administer any accounts.
          {canCreate
            ? " Create an account to get started."
            : " An Account Admin manages account membership and applications."}
        </p>
      )}
      {!selected && !!manageable.length && (
        <TableCard>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Account</TableHead>
                <TableHead>Type</TableHead>
                <TableHead>Status</TableHead>
                <TableHead>Universe quota</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {manageable.map((account) => (
                <TableRow key={account.id}>
                  <TableCell>
                    <Link
                      className="font-medium underline-offset-4 hover:underline"
                      to={`/admin/accounts?account=${encodeURIComponent(account.id)}`}
                    >
                      {account.name}
                    </Link>
                  </TableCell>
                  <TableCell>{kindLabel(account)}</TableCell>
                  <TableCell>{account.enabled ? "Active" : "Suspended"}</TableCell>
                  <TableCell>{account.universeQuota}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </TableCard>
      )}
      {account && <AccountDetails key={account.id} account={account} />}
      {createOpen && (
        <CreateAccountDialog
          close={() => setCreateOpen(false)}
          saved={async (accountId) => {
            await refresh();
            setSearchParams({ account: accountId });
            setCreateOpen(false);
          }}
        />
      )}
    </>
  );
}

function AccountDetails({ account }: { account: Account }) {
  const client = useQueryClient();
  const accountId = account.id;
  const [member, setMember] = useState<Member | "new" | null>(null);
  const [application, setApplication] = useState<Application | "new" | null>(null);
  const [policyOpen, setPolicyOpen] = useState(false);
  const [remove, setRemove] = useState<Member | null>(null);
  const [toggleApp, setToggleApp] = useState<Application | null>(null);
  const members = useQuery({
    queryKey: ["account-members", accountId],
    enabled: account.enabled,
    queryFn: () => platformApi("platform/accounts/members/list", { accountId }),
  });
  const apps = useQuery({
    queryKey: ["applications", accountId],
    enabled: account.enabled,
    queryFn: () => platformApi("platform/accounts/applications/list", { accountId }),
  });
  const refresh = async () => {
    await Promise.all([
      client.invalidateQueries({ queryKey: ["accounts"] }),
      client.invalidateQueries({ queryKey: ["account-members", accountId] }),
      client.invalidateQueries({ queryKey: ["applications", accountId] }),
      client.invalidateQueries({ queryKey: ["universes"] }),
    ]);
  };
  const hasPolicy =
    !!account.membershipPolicy?.members?.length || !!account.membershipPolicy?.directoryGroupMappings?.length;
  return (
    <div className="grid min-w-0 gap-8">
      <section>
        <SectionHeader
          title="Account policy"
          description="Account Admins manage every universe in this account."
          actions={
            <Button variant="outline" onClick={() => setPolicyOpen(true)}>
              <EditIcon data-icon="inline-start" />
              Edit policy
            </Button>
          }
        />
        <Card>
          <CardContent className="grid gap-2 text-sm">
            <p>
              Universe quota: <strong>{account.universeQuota}</strong>
            </p>
            <p>
              New universes:{" "}
              <strong>
                {account.defaultProtected ? "Protected by default" : "Not protected by default"}
              </strong>
            </p>
            <p>
              Protection requirement:{" "}
              <strong>{account.requireProtected ? "Required for all universes" : "Optional"}</strong>
            </p>
          </CardContent>
        </Card>
      </section>
      {!account.enabled ? (
        <p className="text-sm text-muted-foreground">
          This account is suspended. Its people, applications, and universes cannot use it.
        </p>
      ) : (
        <>
          <section className="min-w-0">
            <SectionHeader
              title="People"
              description="Account roles apply across this account. Assign universe roles in Admin → Universes → Access."
              actions={
                <Button onClick={() => setMember("new")}>
                  <AddIcon data-icon="inline-start" />
                  Add member
                </Button>
              }
            />
            {hasPolicy && (
              <p className="mb-3 text-sm text-muted-foreground">
                Account policy also grants membership. Removing a member or lowering their role here does not
                remove access granted by that policy.
              </p>
            )}
            {members.isLoading && <LoadingNote />}
            {members.error && (
              <p role="alert" className="text-destructive">
                {members.error.message}
              </p>
            )}
            {members.data?.members.length === 0 && (
              <p className="text-sm text-muted-foreground">No account members yet. Add an existing user.</p>
            )}
            {!!members.data?.members.length && (
              <TableCard>
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Person</TableHead>
                      <TableHead>Account role</TableHead>
                      <TableHead />
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {members.data.members.map((item) => (
                      <TableRow key={item.id}>
                        <TableCell>
                          <p className="font-medium">{item.name}</p>
                          <p className="text-xs text-muted-foreground">{item.email}</p>
                        </TableCell>
                        <TableCell>{roleLabels[item.role]}</TableCell>
                        <TableCell>
                          <div className="flex justify-end gap-2">
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              aria-label={`Edit account role for ${item.name}`}
                              onClick={() => setMember(item)}
                            >
                              <EditIcon />
                            </Button>
                            <Button
                              variant="ghost"
                              aria-label={`Remove ${item.name}`}
                              onClick={() => setRemove(item)}
                            >
                              Remove
                            </Button>
                          </div>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </TableCard>
            )}
          </section>
          <section className="min-w-0">
            <SectionHeader
              title="Applications"
              description="Applications use their own permissions and API keys. Grant access to specific universes in Admin → Universes → Access."
              actions={
                <Button onClick={() => setApplication("new")}>
                  <AddIcon data-icon="inline-start" />
                  New application
                </Button>
              }
            />
            {apps.isLoading && <LoadingNote />}
            {apps.error && (
              <p role="alert" className="text-destructive">
                {apps.error.message}
              </p>
            )}
            {apps.data?.applications.length === 0 && (
              <p className="text-sm text-muted-foreground">
                No applications yet. Create one for an integration that needs its own identity.
              </p>
            )}
            {!!apps.data?.applications.length && (
              <TableCard>
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Application</TableHead>
                      <TableHead>Account role</TableHead>
                      <TableHead>Status</TableHead>
                      <TableHead />
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {apps.data.applications.map((item) => (
                      <TableRow key={item.id}>
                        <TableCell className="font-medium">{item.name}</TableCell>
                        <TableCell>{roleLabels[item.role]}</TableCell>
                        <TableCell>{item.enabled ? "Active" : "Disabled"}</TableCell>
                        <TableCell>
                          <div className="flex justify-end gap-2">
                            <Button
                              nativeButton={false}
                              variant="outline"
                              render={
                                <Link
                                  to={`/admin/api-keys?account=${encodeURIComponent(accountId)}&application=${encodeURIComponent(item.id)}`}
                                />
                              }
                              aria-label={`API keys for ${item.name}`}
                            >
                              API keys
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              aria-label={`Edit ${item.name}`}
                              onClick={() => setApplication(item)}
                            >
                              <EditIcon />
                            </Button>
                            <Button
                              variant="ghost"
                              aria-label={`${item.enabled ? "Disable" : "Enable"} ${item.name}`}
                              onClick={() => setToggleApp(item)}
                            >
                              {item.enabled ? "Disable" : "Enable"}
                            </Button>
                          </div>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </TableCard>
            )}
          </section>
        </>
      )}
      <p className="text-sm text-muted-foreground">
        For API access acting as you, manage{" "}
        <Link className="underline" to="/account#api-keys">
          your personal API keys
        </Link>
        .
      </p>
      {policyOpen && <PolicyDialog account={account} close={() => setPolicyOpen(false)} saved={refresh} />}
      {member && (
        <MemberDialog
          accountId={accountId}
          member={member === "new" ? undefined : member}
          close={() => setMember(null)}
          saved={refresh}
        />
      )}
      {application && (
        <ApplicationDialog
          accountId={accountId}
          application={application === "new" ? undefined : application}
          close={() => setApplication(null)}
          saved={refresh}
        />
      )}
      {remove && (
        <ConfirmDialog
          title={`Remove ${remove.name}?`}
          description="Remove this person's explicit account membership. Access granted by account policy or a Platform Admin role will remain."
          label="Remove member"
          close={() => setRemove(null)}
          action={async () => {
            await platformApi("platform/accounts/members/delete", {
              accountId,
              userId: remove.userId,
            });
            await refresh();
          }}
        />
      )}
      {toggleApp && (
        <ConfirmDialog
          title={`${toggleApp.enabled ? "Disable" : "Enable"} ${toggleApp.name}?`}
          description={
            toggleApp.enabled
              ? "All of its API keys will stop working. Its universes will remain, and you can enable it again later."
              : "Existing, unexpired API keys will work again with this application's current permissions."
          }
          label={toggleApp.enabled ? "Disable application" : "Enable application"}
          close={() => setToggleApp(null)}
          action={async () => {
            await platformApi("platform/accounts/applications/update", {
              accountId,
              applicationId: toggleApp.id,
              enabled: !toggleApp.enabled,
            });
            await refresh();
          }}
        />
      )}
    </div>
  );
}
