import { useActionPermissions } from "@/lib/permissions";
import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowUpRight } from "lucide-react";
import { Link } from "react-router-dom";
import { api, type Environment } from "@/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { EnvironmentIdlePolicyDialog } from "@/components/environment/idle-policy-dialog";
import {
  EnvironmentPowerControls,
  describeIdlePolicy,
  observedPower,
  powerDiverges,
} from "@/components/environment/power-controls";
import { DetailSection, KeyValue } from "./status";

const GONE = new Set(["closing", "closed", "failed"]);

/**
 * The environment a bot's sessions share: the `existing` environment its
 * profile names. Status, power, and idle policy are shown next to the bot
 * because that is where an operator looks when a bot seems asleep; the
 * policy itself lives on the environment and is edited here or on the
 * Environments page.
 */
export function BotEnvironmentCard({
  slug,
  universeId,
  environmentId,
}: {
  /** Universe slug for page links. */
  slug: string;
  universeId: string;
  environmentId: string;
}) {
  const target = { kind: "environment" as const, id: environmentId };
  const permissions = useActionPermissions(universeId);
  const manage = permissions.can("configure_resource");
  const queryClient = useQueryClient();
  const [policyOpen, setPolicyOpen] = useState(false);
  const environments = useQuery({
    queryKey: ["environments", universeId],
    queryFn: () => api<Environment[]>("GET", `/api/v1/universes/${universeId}/environments`),
    refetchInterval: 15_000,
  });
  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: ["environments", universeId] });
  };
  const current = environments.data?.find((environment) => environment.environmentId === environmentId);
  const provisioned = current?.source.type === "provisioned";

  return (
    <DetailSection
      title="Environment"
      description="The existing environment this bot's profile activates in every session. Shared with anything else that names it."
    >
      {environments.isLoading ? (
        <p className="text-xs text-muted-foreground">Loading…</p>
      ) : environments.error ? (
        <p className="rounded-md bg-destructive/10 p-2 text-xs text-destructive">{environments.error.message}</p>
      ) : !current ? (
        <p className="rounded-md bg-destructive/10 p-2 text-xs text-destructive">
          Environment <code className="font-mono">{environmentId}</code> is not listed in this universe;
          new sessions of this bot will fail to start until the profile points at an open one.
        </p>
      ) : (
        <>
          <KeyValue
            label="Environment"
            value={
              <Link
                to={`/u/${slug}/environments`}
                className="inline-flex items-center gap-1 font-mono text-xs hover:underline"
              >
                {current.displayName ?? current.environmentId}
                <ArrowUpRight className="size-3" />
              </Link>
            }
          />
          <KeyValue
            label="Status"
            value={
              <span className="inline-flex items-center gap-2">
                <Badge variant={GONE.has(current.status) ? "destructive" : current.status === "ready" ? "secondary" : "outline"}>
                  {current.status}
                </Badge>
                {powerDiverges(current) && (
                  <span className="text-xs text-muted-foreground">
                    {observedPower(current) ?? current.status} → {current.desiredPower}
                  </span>
                )}
              </span>
            }
          />
          <KeyValue label="Idle policy" value={describeIdlePolicy(current.idlePolicy ?? undefined)} />
          {manage && !GONE.has(current.status) && (
            <div className="flex flex-wrap items-center gap-2">
              <EnvironmentPowerControls universeId={universeId} environment={current} onChanged={refresh} />
              {provisioned && (
                <Button variant="outline" size="xs" onClick={() => setPolicyOpen(true)}>
                  Idle policy…
                </Button>
              )}
            </div>
          )}
          {GONE.has(current.status) && (
            <p className="rounded-md bg-destructive/10 p-2 text-xs text-destructive">
              This environment is {current.status}: new sessions of this bot cannot start until the
              profile points at an open environment.
            </p>
          )}
          {!GONE.has(current.status) && provisioned && !current.idlePolicy && (
            <p className="rounded-md bg-amber-500/10 p-2 text-xs text-amber-700 dark:text-amber-400">
              No idle policy: this environment never pauses, suspends, or stops while the bot is quiet.
            </p>
          )}
          {!GONE.has(current.status) && !provisioned && (
            <p className="rounded-md border border-dashed p-2 text-xs text-muted-foreground">
              External environment: no power control, so it cannot be paused or stopped while idle.
            </p>
          )}
          {manage && provisioned && (
            <EnvironmentIdlePolicyDialog
              key={`${current.environmentId}:${policyOpen ? "open" : "closed"}`}
              universeId={universeId}
              environment={current}
              open={policyOpen}
              onOpenChange={setPolicyOpen}
              onChanged={refresh}
            />
          )}
        </>
      )}
    </DetailSection>
  );
}
