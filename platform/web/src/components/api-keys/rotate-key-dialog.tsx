import { useMutation } from "@tanstack/react-query";
import type { DeploymentApiKeyCreateResponse, DeploymentApiKeyView } from "@lightspeed-ai/agent-client";
import { api } from "@/api";
import { ApiKeySecret } from "./secret-once";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";

export function RotateKeyDialog({ apiKey, basePath, onClose, onRotated }: {
  apiKey: DeploymentApiKeyView;
  basePath: string;
  onClose: () => void;
  onRotated: () => void;
}) {
  const rotate = useMutation({
    mutationFn: () => api<DeploymentApiKeyCreateResponse>("POST", `${basePath}/${encodeURIComponent(apiKey.keyPrefix)}/rotate`),
    retry: false,
    gcTime: 0,
    onSuccess: onRotated,
  });
  const close = () => {
    if (rotate.isPending) return;
    rotate.reset();
    onClose();
  };
  return <Dialog open onOpenChange={(open) => { if (!open) close(); }}>
    <DialogContent>
      {rotate.data ? <ApiKeySecret secret={rotate.data.secret} keyPrefix={rotate.data.apiKey.keyPrefix} onDone={close} /> : <>
        <DialogHeader>
          <DialogTitle>Rotate this API key?</DialogTitle>
          <DialogDescription>
            The current secret for {apiKey.displayName ?? apiKey.keyPrefix} will stop working immediately.
            Update every client using this key with the new secret. Its permissions stay the same.
          </DialogDescription>
        </DialogHeader>
        {rotate.error && <p role="alert" className="text-sm text-destructive">{rotate.error.message}</p>}
        <DialogFooter>
          <Button variant="outline" disabled={rotate.isPending} onClick={close}>Cancel</Button>
          <Button disabled={rotate.isPending} onClick={() => rotate.mutate()}>{rotate.isPending ? "Rotating…" : "Rotate key"}</Button>
        </DialogFooter>
      </>}
    </DialogContent>
  </Dialog>;
}
