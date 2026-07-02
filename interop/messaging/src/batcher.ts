export interface TurnDebouncerOptions<T> {
  debounceMs: number;
  maxBatch: number;
  maxWaitMs: number;
  onFlush: (key: string, batch: T[]) => void;
}

/// Collects rapid consecutive user turns per conversation into one batch.
/// A batch flushes when the chat has been quiet for `debounceMs`, when it
/// reaches `maxBatch` entries, or when the oldest entry has waited
/// `maxWaitMs`.
export class TurnDebouncer<T> {
  private readonly pending = new Map<
    string,
    { batch: T[]; firstAtMs: number; timer: NodeJS.Timeout }
  >();

  constructor(private readonly options: TurnDebouncerOptions<T>) {}

  push(key: string, item: T): void {
    const existing = this.pending.get(key);
    if (!existing) {
      const entry = {
        batch: [item],
        firstAtMs: Date.now(),
        timer: setTimeout(() => this.flush(key), this.options.debounceMs),
      };
      entry.timer.unref?.();
      this.pending.set(key, entry);
      return;
    }
    existing.batch.push(item);
    if (existing.batch.length >= this.options.maxBatch) {
      this.flush(key);
      return;
    }
    clearTimeout(existing.timer);
    const elapsed = Date.now() - existing.firstAtMs;
    const remainingMaxWait = Math.max(0, this.options.maxWaitMs - elapsed);
    existing.timer = setTimeout(
      () => this.flush(key),
      Math.min(this.options.debounceMs, remainingMaxWait),
    );
    existing.timer.unref?.();
  }

  flush(key: string): void {
    const entry = this.pending.get(key);
    if (!entry) {
      return;
    }
    clearTimeout(entry.timer);
    this.pending.delete(key);
    this.options.onFlush(key, entry.batch);
  }

  flushAll(): void {
    for (const key of [...this.pending.keys()]) {
      this.flush(key);
    }
  }

  pendingCount(key: string): number {
    return this.pending.get(key)?.batch.length ?? 0;
  }

  /// Items currently queued for `key` and not yet flushed. Exposed so callers
  /// can avoid acting on state a queued item still references.
  pendingItems(key: string): readonly T[] {
    return this.pending.get(key)?.batch ?? [];
  }
}
