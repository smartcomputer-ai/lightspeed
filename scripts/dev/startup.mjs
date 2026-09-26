// Readiness gates run before dependent application processes are spawned.
import net from "node:net";
import { setTimeout as delay } from "node:timers/promises";

export async function startProcesses(processes, { start, wait = waitForService }) {
  for (const processPlan of processes) {
    if (processPlan.startAfter) {
      console.log(`[startup] waiting for ${processPlan.startAfter.name} before ${processPlan.name}`);
      await wait(processPlan.startAfter);
    }
    start(processPlan);
  }
}

export async function waitForService(service, {
  isStopping = () => false,
  timeoutMs = 60_000,
  pollMs = 250,
} = {}) {
  const deadline = Date.now() + timeoutMs;
  while (!isStopping() && Date.now() < deadline) {
    const ready = service.url ? await httpUp(service.url) : await tcpUp(service.port);
    if (ready) return;
    await delay(pollMs);
  }
  throw new Error(`${service.name} did not become ready within ${timeoutMs / 1_000} seconds`);
}

async function httpUp(url) {
  try {
    const response = await fetch(url, { signal: AbortSignal.timeout(1_000) });
    return response.ok;
  } catch {
    return false;
  }
}

export function tcpUp(port) {
  return new Promise((resolve) => {
    const socket = net.connect({ port, host: "127.0.0.1", timeout: 500 });
    socket.once("connect", () => {
      socket.destroy();
      resolve(true);
    });
    socket.once("error", () => resolve(false));
    socket.once("timeout", () => {
      socket.destroy();
      resolve(false);
    });
  });
}
