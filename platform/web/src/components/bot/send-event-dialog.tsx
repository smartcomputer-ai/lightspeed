import { useState, type FormEvent } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useActionPermissions } from "@/lib/permissions";
import { api } from "@/api";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";

export function SendEventDialog({
  universeId,
  botId,
  open,
  onOpenChange,
}: {
  universeId: string;
  botId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const permissions = useActionPermissions(universeId, [{ kind: "bot", id: botId }]);
  const invoke = permissions.can("invoke_bot", { kind: "bot", id: botId });
  const queryClient = useQueryClient();
  const [kind, setKind] = useState("operator.requested");
  const [summary, setSummary] = useState("");
  const [data, setData] = useState("");
  const [error, setError] = useState<string | null>(null);
  const send = useMutation({
    mutationFn: () => {
      if (!invoke) throw new Error("You do not have permission to invoke this bot.");
      let parsed: unknown = undefined;
      if (data.trim()) parsed = JSON.parse(data);
      return api("POST", `/api/v1/universes/${universeId}/bots/${botId}/events`, {
        event: {
          kind: kind.trim(),
          summary: summary.trim(),
          ...(parsed === undefined ? {} : { data: parsed }),
        },
      });
    },
    onSuccess: async () => {
      setSummary("");
      setData("");
      setError(null);
      onOpenChange(false);
      await queryClient.invalidateQueries({ queryKey: ["bot-state", universeId, botId] });
      await queryClient.invalidateQueries({ queryKey: ["bot-events", universeId, botId] });
    },
    onError: (err) => setError(err instanceof Error ? err.message : String(err)),
  });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    send.mutate();
  };
  return (
    <Dialog open={open && invoke} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Send event</DialogTitle>
          <DialogDescription>
            Queue a durable event for this bot. It is delivered to the bot's session as untrusted
            input.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <Field>
            <FieldLabel htmlFor="bot-event-kind">Kind</FieldLabel>
            <Input id="bot-event-kind" value={kind} onChange={(event) => setKind(event.target.value)} />
          </Field>
          <Field>
            <FieldLabel htmlFor="bot-event-summary">Summary</FieldLabel>
            <Textarea
              id="bot-event-summary"
              value={summary}
              onChange={(event) => setSummary(event.target.value)}
              rows={4}
              autoFocus
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="bot-event-data">Data (optional JSON)</FieldLabel>
            <Textarea
              id="bot-event-data"
              value={data}
              onChange={(event) => setData(event.target.value)}
              rows={5}
              className="font-mono text-xs"
            />
          </Field>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={!invoke || send.isPending || !kind.trim() || !summary.trim()}>
              {send.isPending ? "Sending…" : "Send event"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
