import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { readEventStream, type AgentNotification } from "@lightspeed-ai/sdk";
import { useActiveUniverse } from "./universes";
import { WatchCache } from "./watch-cache";

const WatchContext = createContext<{ universeId?: string; connected: boolean }>({ connected: false });

/** One connection for the active universe in this tab, shared across its pages. */
export function UniverseWatchProvider({ children }: { children: ReactNode }) {
  const { universe } = useActiveUniverse();
  const universeId = universe?.id;
  const features = JSON.stringify(universe?.features);
  const role = universe?.role;
  const client = useQueryClient();
  const [connection, setConnection] = useState<{ universeId?: string; connected: boolean }>({ connected: false });
  useEffect(() => {
    if (!universeId) { setConnection({ connected: false }); return; }
    const controller = new AbortController();
    const cache = new WatchCache(client, universeId);
    let timer: ReturnType<typeof setTimeout> | undefined;
    let attempts = 0;
    const connect = async () => {
      try {
        const admission = new AbortController();
        const deadline = setTimeout(() => admission.abort(), 15_000);
        const response = await fetch(`/api/v1/universes/${encodeURIComponent(universeId)}/watch`, {
          credentials: "same-origin", headers: { accept: "text/event-stream" }, signal: AbortSignal.any([controller.signal, admission.signal]),
        }).finally(() => clearTimeout(deadline));
        if (!response.ok || !response.body || !response.headers.get("content-type")?.startsWith("text/event-stream")) {
          await response.body?.cancel();
          if ([401, 403, 404].includes(response.status)) {
            void client.invalidateQueries({ queryKey: ["universes"] });
            return;
          }
          throw new Error("Watch unavailable");
        }
        for await (const data of readEventStream(response.body)) {
          if (controller.signal.aborted) return;
          const event = JSON.parse(data) as AgentNotification;
          if (event.method === "watch/reset") {
            attempts = 0;
            setConnection({ universeId, connected: true });
          }
          cache.receive(event);
        }
      } catch {
        // Ordinary polling remains available throughout outages.
      } finally {
        if (!controller.signal.aborted) setConnection({ universeId, connected: false });
      }
      if (!controller.signal.aborted) timer = setTimeout(() => void connect(), Math.min(30_000, 1_000 * 2 ** Math.min(attempts++, 5)));
    };
    void connect();
    return () => { controller.abort(); clearTimeout(timer); cache.dispose(); };
  }, [client, universeId, features, role]);
  return <WatchContext.Provider value={connection}>{children}</WatchContext.Provider>;
}

export function useWatchConnected(universeId: string): boolean {
  const connection = useContext(WatchContext);
  return connection.universeId === universeId && connection.connected;
}
