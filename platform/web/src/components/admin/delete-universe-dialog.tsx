import { useState } from "react";
import {
  AlertDialog,
  AlertDialogTrigger,
  AlertDialogContent,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogCancel,
  AlertDialogAction,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";

export function DeleteUniverseDialog({
  name,
  handle,
  protected: protectedValue,
  pending,
  onDelete,
}: {
  name: string;
  handle: string;
  protected?: boolean;
  pending?: boolean;
  onDelete: () => void;
}) {
  const [confirmation, setConfirmation] = useState("");
  return (
    <AlertDialog onOpenChange={() => setConfirmation("")}>
      <AlertDialogTrigger
        render={
          <Button
            variant="ghost"
            size="sm"
            className="text-destructive"
            disabled={protectedValue || pending}
            title={
              protectedValue
                ? "Clear deletion protection in universe settings first"
                : undefined
            }
          />
        }
      >
        Delete
      </AlertDialogTrigger>
      <AlertDialogContent className="max-h-[calc(100dvh-2rem)] overflow-y-auto">
        <AlertDialogHeader>
          <AlertDialogTitle>Permanently delete {name}?</AlertDialogTitle>
          <AlertDialogDescription>
            Stops this universe immediately and removes its resources, members
            and stored content in the background. This cannot be undone.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <Field>
          <FieldLabel htmlFor={`delete-${handle}`}>
            Type {handle} to confirm
          </FieldLabel>
          <Input
            id={`delete-${handle}`}
            value={confirmation}
            onChange={(event) => setConfirmation(event.target.value)}
            autoComplete="off"
          />
        </Field>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction
            className="bg-destructive text-white hover:bg-destructive/90"
            disabled={confirmation !== handle || pending}
            onClick={onDelete}
          >
            Delete permanently
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
