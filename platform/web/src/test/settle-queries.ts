import { act } from "react";
import type { QueryClient } from "@tanstack/react-query";
import { vi } from "vitest";

/** Under fake timers, advance until every query and mutation of `client` has
 * settled. A fixed number of steps quietly stops short once a page chains
 * one more request; this keeps going, and fails clearly if work never ends.
 * Takes at least `minSteps`, so timers that are not requests (debounces)
 * still run. `pending` is for a test that holds a request open on purpose:
 * it takes the minimum steps without waiting for the client to go idle. */
export async function settleQueries(client: QueryClient, { minSteps = 5, pending = false } = {}): Promise<void> {
  const maxSteps = 50;
  for (let step = 1; step <= maxSteps; step++) {
    await act(async () => { await vi.advanceTimersByTimeAsync(5); });
    if (step >= minSteps && (pending || (client.isFetching() === 0 && client.isMutating() === 0))) return;
  }
  throw new Error(`queries or mutations still running after ${maxSteps} steps`);
}
