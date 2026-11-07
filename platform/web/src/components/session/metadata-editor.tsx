/// Key/value rows for a session's descriptive metadata. Rows are edited
/// freely; `rowsToMetadata` drops incomplete rows and the last duplicate key
/// wins, which is exactly what the put will store.
import { useEffect, useState } from "react";
import { AddIcon, RemoveIcon } from "@/icons";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

export interface MetadataRow {
  key: string;
  value: string;
}

/// Bounds mirrored from the engine's metadata validator; the server rejects
/// anything past them, the editor just stops the user earlier.
export const METADATA_MAX_ENTRIES = 32;
export const METADATA_MAX_KEY_BYTES = 64;
export const METADATA_MAX_VALUE_BYTES = 256;

export function metadataToRows(metadata: Record<string, string> | undefined): MetadataRow[] {
  return Object.entries(metadata ?? {}).map(([key, value]) => ({ key, value }));
}

export function rowsToMetadata(rows: MetadataRow[]): Record<string, string> {
  const metadata: Record<string, string> = {};
  for (const row of rows) {
    const key = row.key.trim();
    const value = row.value.trim();
    if (key && value) metadata[key] = value;
  }
  return metadata;
}

export function sameMetadata(a: Record<string, string>, b: Record<string, string>): boolean {
  const aKeys = Object.keys(a).sort();
  const bKeys = Object.keys(b).sort();
  return aKeys.length === bKeys.length && aKeys.every((key, i) => key === bKeys[i] && a[key] === b[key]);
}

/** Controlled metadata map with row state that preserves an unfinished pair. */
export function MetadataMapEditor({
  value,
  onChange,
  disabled,
}: {
  value?: Record<string, string>;
  onChange: (metadata: Record<string, string> | undefined) => void;
  disabled?: boolean;
}) {
  const source = JSON.stringify(value ?? {});
  const [rows, setRows] = useState<MetadataRow[]>(() => metadataToRows(value));
  const [syncedSource, setSyncedSource] = useState(source);

  useEffect(() => {
    if (source !== syncedSource) {
      setRows(metadataToRows(value));
      setSyncedSource(source);
    }
  }, [source, syncedSource, value]);

  const changeRows = (next: MetadataRow[]) => {
    const metadata = rowsToMetadata(next);
    setRows(next);
    setSyncedSource(JSON.stringify(metadata));
    onChange(Object.keys(metadata).length ? metadata : undefined);
  };

  return <MetadataEditor rows={rows} onChange={changeRows} disabled={disabled} />;
}

export function MetadataEditor({
  rows,
  onChange,
  disabled,
}: {
  rows: MetadataRow[];
  onChange: (rows: MetadataRow[]) => void;
  disabled?: boolean;
}) {
  const update = (index: number, patch: Partial<MetadataRow>) =>
    onChange(rows.map((row, i) => (i === index ? { ...row, ...patch } : row)));
  return (
    <div className="grid min-w-0 max-w-full gap-2">
      {rows.map((row, index) => (
        <div key={index} className="flex min-w-0 items-center gap-2">
          <Input
            value={row.key}
            onChange={(event) => update(index, { key: event.target.value })}
            placeholder="key"
            aria-label={`Metadata key ${index + 1}`}
            className="font-mono text-xs"
            maxLength={METADATA_MAX_KEY_BYTES}
            disabled={disabled}
          />
          <Input
            value={row.value}
            onChange={(event) => update(index, { value: event.target.value })}
            placeholder="value"
            aria-label={`Metadata value ${index + 1}`}
            className="font-mono text-xs"
            maxLength={METADATA_MAX_VALUE_BYTES}
            disabled={disabled}
          />
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label={`Remove metadata row ${index + 1}`}
            disabled={disabled}
            onClick={() => onChange(rows.filter((_, i) => i !== index))}
          >
            <RemoveIcon />
          </Button>
        </div>
      ))}
      <div>
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={disabled || rows.length >= METADATA_MAX_ENTRIES}
          onClick={() => onChange([...rows, { key: "", value: "" }])}
        >
          <AddIcon /> Add pair
        </Button>
      </div>
    </div>
  );
}
