import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

export function ApiKeyRevokeDialog({
  name,
  owner,
  pending,
  error,
  onConfirm,
  onClose,
}: {
  name?: string;
  owner?: string;
  pending: boolean;
  error?: Error | null;
  onConfirm: () => void;
  onClose: () => void;
}) {
  return (
    <Dialog
      open={name !== undefined}
      onOpenChange={(open) => {
        if (!open && !pending) onClose();
      }}
    >
      <DialogContent
        className="max-h-[min(92dvh,800px)] overflow-y-auto sm:max-w-md"
        showCloseButton={!pending}
      >
        <DialogHeader>
          <DialogTitle>Revoke API key</DialogTitle>
          <DialogDescription>
            Revoke “{name}”{owner ? ` for ${owner}` : ""}? Clients using it will
            lose access. Create and deploy a replacement first if they need
            uninterrupted access.
          </DialogDescription>
        </DialogHeader>
        {error && (
          <p role="alert" className="text-sm text-destructive">
            {error.message}
          </p>
        )}
        <DialogFooter>
          <Button variant="outline" disabled={pending} onClick={onClose}>
            Cancel
          </Button>
          <Button variant="destructive" disabled={pending} onClick={onConfirm}>
            {pending ? "Revoking…" : "Revoke key"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
