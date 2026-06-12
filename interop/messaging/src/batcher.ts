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
}

export interface RoomEventItem {
  key: string;
  text: string;
}

export interface RoomBufferOptions {
  flushMs: number;
  flushMax: number;
  budget: number;
  onFlush: (key: string, events: RoomEventItem[], dropped: number) => Promise<void>;
  log?: (message: string) => void;
}

/// Buffers unaddressed room chatter per conversation and flushes it in
/// batches: every `flushMs`, at `flushMax` buffered events, or on `drain`
/// (called before a user turn runs so the context lands first). At most
/// `budget` events are kept between flushes; older overflow is dropped and
/// reported through the flush callback.
export class RoomBuffer {
  private readonly buffers = new Map<
    string,
    { events: RoomEventItem[]; dropped: number; timer: NodeJS.Timeout | null }
  >();
  private readonly flushing = new Map<string, Promise<void>>();

  constructor(private readonly options: RoomBufferOptions) {}

  push(key: string, event: RoomEventItem): void {
    let buffer = this.buffers.get(key);
    if (!buffer) {
      buffer = { events: [], dropped: 0, timer: null };
      this.buffers.set(key, buffer);
    }
    buffer.events.push(event);
    const overflow = buffer.events.length - this.options.budget;
    if (overflow > 0) {
      buffer.events.splice(0, overflow);
      buffer.dropped += overflow;
    }
    if (buffer.events.length >= this.options.flushMax) {
      void this.drain(key);
      return;
    }
    if (!buffer.timer) {
      buffer.timer = setTimeout(() => {
        void this.drain(key);
      }, this.options.flushMs);
      buffer.timer.unref?.();
    }
  }

  bufferedCount(key: string): number {
    return this.buffers.get(key)?.events.length ?? 0;
  }

  /// Flushes the buffer for `key` and resolves when the flush callback is
  /// done. Serialized per key; failed flushes re-buffer the events so the
  /// next flush retries them.
  async drain(key: string): Promise<void> {
    const previous = this.flushing.get(key) ?? Promise.resolve();
    const next = previous.catch(() => undefined).then(() => this.flushNow(key));
    this.flushing.set(key, next);
    try {
      await next;
    } finally {
      if (this.flushing.get(key) === next) {
        this.flushing.delete(key);
      }
    }
  }

  async drainAll(): Promise<void> {
    await Promise.all([...this.buffers.keys()].map((key) => this.drain(key)));
  }

  private async flushNow(key: string): Promise<void> {
    const buffer = this.buffers.get(key);
    if (!buffer || buffer.events.length === 0) {
      return;
    }
    if (buffer.timer) {
      clearTimeout(buffer.timer);
      buffer.timer = null;
    }
    const events = buffer.events;
    const dropped = buffer.dropped;
    buffer.events = [];
    buffer.dropped = 0;
    try {
      await this.options.onFlush(key, events, dropped);
    } catch (error) {
      // Re-buffer at the front so ordering survives a failed flush; the
      // budget still applies on the next push.
      buffer.events = [...events, ...buffer.events];
      buffer.dropped += dropped;
      this.options.log?.(
        `bridge: room flush failed for ${key}: ${error instanceof Error ? error.message : String(error)}`,
      );
      throw error;
    }
  }
}
