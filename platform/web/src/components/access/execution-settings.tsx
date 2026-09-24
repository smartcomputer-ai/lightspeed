import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { AccessExecutionReadResponse } from "@lightspeed-ai/agent-client";
import { api } from "@/api";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { ReadError } from "@/components/read-error";
import { DEFAULT_AGENT_IDENTITY } from "./shared";

export function ExecutionSettings({ universeId }: { universeId: string }) {
  const client = useQueryClient();
  const policy = useQuery({
    queryKey: ["execution-policy", universeId],
    queryFn: () =>
      api<AccessExecutionReadResponse>(
        "GET",
        `/api/v1/universes/${universeId}/access/execution`,
      ),
  });
  const update = useMutation({
    mutationFn: (enabled: boolean) =>
      api<AccessExecutionReadResponse>(
        "PUT",
        `/api/v1/universes/${universeId}/access/execution`,
        { personalExecutionEnabled: enabled },
      ),
    onSuccess: (value) =>
      client.setQueryData(["execution-policy", universeId], value),
  });
  return (
    <Card>
      <CardHeader>
        <CardTitle>Execution</CardTitle>
        <CardDescription>
          Sessions and bots run as {DEFAULT_AGENT_IDENTITY} unless personal
          execution lets people run them as themselves. Existing sessions and
          bots keep their execution identity.
        </CardDescription>
      </CardHeader>
      <CardContent className="grid justify-items-start gap-3">
        {policy.error && <ReadError error={policy.error} />}
        {!policy.data && !policy.error && (
          <p className="text-sm text-muted-foreground">
            Loading execution policy…
          </p>
        )}
        {policy.data && (
          <>
            <p className="text-sm">
              Personal execution is{" "}
              {policy.data.policy.personalExecutionEnabled
                ? "enabled"
                : "disabled"}
              .
            </p>
            <Button
              variant="outline"
              disabled={update.isPending}
              onClick={() =>
                update.mutate(!policy.data.policy.personalExecutionEnabled)
              }
            >
              {update.isPending
                ? "Saving…"
                : policy.data.policy.personalExecutionEnabled
                  ? "Disable personal execution"
                  : "Enable personal execution"}
            </Button>
          </>
        )}
        {update.error && (
          <p role="alert" className="text-sm text-destructive">
            {update.error.message}
          </p>
        )}
      </CardContent>
    </Card>
  );
}
