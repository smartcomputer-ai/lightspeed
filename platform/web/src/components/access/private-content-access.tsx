import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { api, type Member } from "@/api";
import { Button } from "@/components/ui/button";

/** A capability is separate from a role; each button makes one audited change. */
export function PrivateContentAccess({
  universeId,
  member,
  onDone,
}: {
  universeId: string;
  member: Member;
  onDone: () => Promise<unknown>;
}) {
  const [enabled, setEnabled] = useState(member.readPrivateContent === true);
  const change = useMutation({
    mutationFn: () =>
      api<{ enabled: boolean }>(
        "PUT",
        `/api/v1/universes/${universeId}/members/${encodeURIComponent(member.id)}/private-content-access`,
        { enabled: !enabled },
      ),
    onSuccess: async (result) => {
      setEnabled(result.enabled);
      await onDone();
    },
  });
  if (member.subject?.kind !== "principal" || member.principalKind !== "user")
    return null;
  return (
    <section className="grid gap-2 border-t pt-4">
      <h3 className="text-sm font-medium">Private-content access</h3>
      <p className="text-sm text-muted-foreground">
        {enabled ? "Allowed" : "Not allowed"}. This person can{" "}
        {enabled ? "" : "be allowed to "}read restricted sessions, bots and
        collections without a share. Each such read is audited. This grants no
        control or ownership.
      </p>
      <Button
        type="button"
        variant="outline"
        className="justify-self-start"
        disabled={change.isPending}
        onClick={() => change.mutate()}
      >
        {change.isPending
          ? "Saving…"
          : enabled
            ? "Remove private-content access"
            : "Allow private-content reads"}
      </Button>
      <p className="text-xs text-muted-foreground">
        Changes apply immediately, independently of the role above.
      </p>
      {change.error && (
        <p role="alert" className="text-sm text-destructive">
          {change.error.message}
        </p>
      )}
    </section>
  );
}
