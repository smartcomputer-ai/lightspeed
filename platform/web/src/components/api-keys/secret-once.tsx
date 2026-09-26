import { useState } from "react";
import { Check, Copy } from "lucide-react";
import { Button } from "@/components/ui/button";
import { DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";

/** A newly minted key's secret, shown once inside its create dialog. */
export function ApiKeySecret({ secret, keyPrefix, onDone }: { secret: string; keyPrefix: string; onDone: () => void }) {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    await navigator.clipboard.writeText(secret);
    setCopied(true);
  };
  return (
    <>
      <DialogHeader>
        <DialogTitle>Copy your API key</DialogTitle>
        <DialogDescription>
          This secret is shown once. Store it in the client's secret manager before closing this
          dialog.
        </DialogDescription>
      </DialogHeader>
      <div className="grid gap-2">
        <FieldLabel htmlFor="api-key-secret">API key</FieldLabel>
        <div className="flex gap-2">
          <Input
            id="api-key-secret"
            value={secret}
            readOnly
            className="font-mono text-xs"
            onFocus={(event) => event.currentTarget.select()}
          />
          <Button type="button" variant="outline" onClick={() => void copy()}>
            {copied ? <Check data-icon="inline-start" /> : <Copy data-icon="inline-start" />}
            {copied ? "Copied" : "Copy"}
          </Button>
        </div>
        <p className="text-xs text-muted-foreground">
          Identifier: <span className="font-mono">{keyPrefix}</span>
        </p>
      </div>
      <DialogFooter>
        <Button type="button" onClick={onDone}>I saved the key</Button>
      </DialogFooter>
    </>
  );
}
