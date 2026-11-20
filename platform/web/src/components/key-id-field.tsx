import { KEY_ID_MAX_LENGTH } from "@lightspeed-ai/platform-shared";
import { ApiError } from "@/api";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { SettingsDisclosure } from "@/components/ui/settings-disclosure";

/** Whether a create was refused because the id chosen for the key is taken. */
export function isKeyIdConflict(error: unknown, keyId: string): boolean {
  return error instanceof ApiError && error.status === 409 && keyId.trim() !== "";
}

/**
 * The id of a key being created. One is generated unless its creator opens
 * the field and types their own; `conflict` reopens the field when that id
 * was refused as taken. The dialog shows the refusal with its other errors.
 */
export function KeyIdField({
  id,
  value,
  onChange,
  conflict,
}: {
  id: string;
  value: string;
  onChange: (value: string) => void;
  conflict: boolean;
}) {
  const keyId = value.trim();
  return (
    <SettingsDisclosure
      summary={keyId ? `Key ID ${keyId}` : "Generated key ID"}
      action="Change"
      label="Change key ID"
      forceOpen={conflict}
    >
      <Field>
        <FieldLabel htmlFor={id}>Custom key ID</FieldLabel>
        <Input
          id={id}
          value={value}
          onChange={(event) => onChange(event.target.value)}
          placeholder="Generated automatically when blank"
          maxLength={KEY_ID_MAX_LENGTH}
          aria-invalid={conflict || undefined}
          className="font-mono"
        />
        <FieldDescription>
          Configuration documents and the CLI address the key by this ID. It cannot be changed later.
        </FieldDescription>
      </Field>
    </SettingsDisclosure>
  );
}
