import type { Persistence } from "./setting";

/// Where a provider keeps a stored value: `local` for synced and device
/// settings, `tab` for tab settings.
export type StorageArea = "local" | "tab";

export interface StoredChange {
  area: StorageArea;
  key: string;
  /// The stored text, or null to remove it.
  raw: string | null;
  /// The persistence of the setting being written. A provider that saves to
  /// the account sends only `synced` changes there.
  persistence: Persistence;
  /// How long the provider may hold the write before saving it.
  delayMs?: number;
}

/// Storage behind the settings store. Values are text by storage key; the
/// store owns the format, fields and defaults.
///
/// The first provider keeps everything in browser storage. A provider that
/// saves synced settings to the user's account wraps browser storage as its
/// cache, so `peek` still answers at once for first paint.
export interface SettingsProvider {
  /// Resolves once the provider has loaded its values.
  readonly ready: Promise<void>;
  /// The stored text now, from the provider's local copy; null when nothing
  /// is stored or storage cannot be read.
  peek(area: StorageArea, key: string): string | null;
  /// Saves changes; resolves when they are stored, rejects when they cannot be.
  save(changes: readonly StoredChange[]): Promise<void>;
  /// Saves any writes still held back.
  flush(): Promise<void>;
  /// Drops the writes still held back without saving them; their saves
  /// reject. For tests, which start each case on a fresh page.
  discard(): void;
  /// Values changed somewhere else: another tab now, the account later.
  /// Null means anything may have changed (storage was cleared).
  subscribe(onChange: (changed: ReadonlyArray<{ area: StorageArea; key: string }> | null) => void): () => void;
}
