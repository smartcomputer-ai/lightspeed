// Readiness gates run before dependent application processes are spawned.
import net from "node:net";
import { setTimeout as delay } from "node:timers/promises";
import { DevError, connectionProblem } from "./errors.mjs";

export async function startProcesses(processes, { start, wait = waitForService, isStopping = () => false }) {
  for (const processPlan of processes) {
    if (isStopping()) return;
    if (processPlan.startAfter) {
      console.log(`[startup] waiting for ${processPlan.startAfter.name} before ${processPlan.name}`);
      await wait(processPlan.startAfter);
    }
    if (isStopping()) return;
    start(processPlan);
  }
}

export async function waitForService(service, {
  isStopping = () => false,
  timeoutMs = 60_000,
  pollMs = 250,
} = {}) {
  const deadline = Date.now() + timeoutMs;
  const endpoint = service.url ?? `127.0.0.1:${service.port}`;
  let lastResult = "No successful check.";
  while (!isStopping() && Date.now() < deadline) {
    if (service.url) {
      try {
        const response = await fetch(service.url, { signal: AbortSignal.timeout(1_000) });
        await response.body?.cancel();
        if (response.ok) return;
        lastResult = `HTTP ${response.status} ${response.statusText}`.trim();
      } catch (error) {
        lastResult = connectionProblem(error);
      }
    } else if (await tcpUp(service.port)) return;
    else lastResult = "TCP connection could not be established.";
    await delay(pollMs);
  }
  if (isStopping()) return;
  throw new DevError(`${service.name} did not become ready within ${timeoutMs / 1_000} seconds.`, {
    details: [`Endpoint: ${endpoint}`, `Waited: ${((Date.now() - deadline + timeoutMs) / 1_000).toFixed(1)}s`, `Last check: ${lastResult}`],
    hint: "Check the service output above and the configured endpoint. It may still be compiling, have failed during startup, or be listening elsewhere.",
  });
}

export function tcpUp(port, host = "127.0.0.1") {
  return new Promise((resolve) => {
    const socket = net.connect({ port, host, timeout: 500 });
    socket.once("connect", () => {
      socket.destroy();
      resolve(true);
    });
    socket.once("error", () => { socket.destroy(); resolve(false); });
    socket.once("timeout", () => {
      socket.destroy();
      resolve(false);
    });
  });
}
