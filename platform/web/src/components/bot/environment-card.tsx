import { useActionPermissions } from "@/lib/permissions";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowUpRight } from "lucide-react";
import { Link } from "react-router-dom";
import { Badge } from "@/components/ui/badge";
import { EnvironmentPowerControls } from "@/components/environment/power-controls";
import {
  describeIdlePolicy,
  environmentTemplate,
  environmentTemplatesQuery,
  lifecycleMessage,
  openEnvironmentsQuery,
} from "@/lib/environments";
import { DetailSection, KeyValue } from "./status";

const GONE = new Set(["closing", "closed", "failed"]);

/**
 * The environment a bot's sessions share: the default environment its
 * profile attaches by id. Status and power are shown next to the bot
 * because that is where an operator looks when a bot seems asleep; the
 * provider runs the idle policy its template sets.
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
  const permissions = useActionPermissions(universeId);
  const manage = permissions.can("configure_resource");
  const queryClient = useQueryClient();
  const environments = useQuery({ ...openEnvironmentsQuery(universeId), refetchInterval: 15_000 });
  const templates = useQuery(environmentTemplatesQuery(universeId));
  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: ["environments", universeId] });
  };
  const current = environments.data?.find((environment) => environment.environmentId === environmentId);
  const provisioned = current?.source.type === "provisioned";
  const template = current ? environmentTemplate(current, templates.data) : undefined;
  const message = current ? lifecycleMessage(current) : null;
  const idle = current?.source.type === "provisioned" ? current.source.idle ?? template?.idle : undefined;

  return (
    <DetailSection
      title="Environment"
      description="The default environment this bot's profile attaches in every session. Shared with anything else that names it."
    >
      {environments.isLoading ? (
        <p className="text-xs text-muted-foreground">Loading…</p>
      ) : environments.error ? (
        <p className="rounded-md bg-destructive/10 p-2 text-xs text-destructive">{environments.error.message}</p>
      ) : !current ? (
        <p className="rounded-md bg-destructive/10 p-2 text-xs text-destructive">
          Environment <code className="font-mono">{environmentId}</code> is closed or not listed in this
          universe; new sessions of this bot will fail to start until the profile points at an open one.
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
              <Badge variant={GONE.has(current.status) ? "destructive" : current.status === "ready" ? "secondary" : "outline"}>
                {current.status}
              </Badge>
            }
          />
          {provisioned && (
            <KeyValue label="Idle policy" value={describeIdlePolicy(idle)} />
          )}
          {message && <p className="text-xs text-muted-foreground">{message}</p>}
          {manage && !GONE.has(current.status) && (
            <div className="flex flex-wrap items-center gap-2">
              <EnvironmentPowerControls
                universeId={universeId}
                environment={current}
                powerStates={template?.powerStates ?? []}
                onChanged={refresh}
              />
            </div>
          )}
          {GONE.has(current.status) && (
            <p className="rounded-md bg-destructive/10 p-2 text-xs text-destructive">
              This environment is {current.status}: new sessions of this bot cannot start until the
              profile points at an open environment.
            </p>
          )}
          {!GONE.has(current.status) && !provisioned && (
            <p className="rounded-md border border-dashed p-2 text-xs text-muted-foreground">
              {current.source.type === "registered" ? "Registered" : "External"} environment: its
              own infrastructure runs it, so Lightspeed has no power control over it.
            </p>
          )}
        </>
      )}
    </DetailSection>
  );
}
