import { spawn } from "node:child_process";
import { stripVTControlCharacters } from "node:util";
import { DevError, redact } from "./errors.mjs";

export function launchCommand(plan, { captureStdout = false, write = (text) => process.stdout.write(text) } = {}) {
  const child = spawn(plan.command, plan.args, {
    cwd: plan.cwd, env: plan.env, shell: false,
    // Stop the command and its watchers/subprocesses as one group on Unix.
    detached: process.platform !== "win32",
  });
  let tail = "";
  let stdout = "";
  let spawnError;
  const record = { child, name: plan.name, closed: false, tail: () => tail };
  for (const [stream, hidden] of [[child.stdout, captureStdout], [child.stderr, false]]) {
    let pending = "";
    stream.setEncoding("utf8");
    stream.on("data", (chunk) => {
      if (hidden) {
        stdout += chunk;
        return;
      }
      tail = (tail + stripVTControlCharacters(chunk)).slice(-12_000);
      pending += chunk;
      const lines = pending.split("\n");
      pending = lines.pop();
      for (const line of lines) write(`[${plan.name}] ${line}\n`);
      // Bound buffering even for programs which never print a newline.
      if (pending.length > 12_000) {
        write(`[${plan.name}] ${pending}\n`);
        pending = "";
      }
    });
    stream.on("end", () => { if (pending) write(`[${plan.name}] ${pending}\n`); });
  }
  record.done = new Promise((resolve, reject) => {
    let drainTimer;
    child.once("error", (error) => { spawnError = error; });
    const finish = (code, signal) => {
      if (record.closed) return;
      clearTimeout(drainTimer);
      record.closed = true;
      if (spawnError || code !== 0) {
        reject(commandFailure(plan, { code, signal, cause: spawnError, tail }));
      } else resolve({ stdout });
    };
    child.once("close", finish);
    // A crashed watcher can leave descendants holding its output pipes open.
    // Give final output time to drain without waiting forever for those pipes.
    child.once("exit", (code, signal) => {
      drainTimer = setTimeout(() => finish(code, signal), 100);
    });
  });
  return record;
}

export function commandFailure(plan, { code, signal, cause, tail = "" } = {}) {
  const observation = cause
    ? `${cause.code ?? "Spawn error"}: ${cause.message}`
    : signal ? `Terminated by ${signal}.` : `Exited with code ${code}.`;
  const details = [
    `Command: ${[plan.command, ...plan.args].join(" ")}`,
    `Directory: ${plan.cwd}`,
    observation,
  ];
  if (tail.trim()) details.push("Recent output:", ...tail.trimEnd().split("\n").slice(-20).map((line) => `  ${line}`));
  let hint = "Inspect the recent output above, fix the reported error, and retry.";
  if (cause?.code === "ENOENT") hint = "Check that the command is installed and its working directory exists. For workspace tools, run npm install from the repository root.";
  else if (["EACCES", "EPERM"].includes(cause?.code)) hint = "Check executable and directory permissions, and whether your environment permits starting this command.";
  else if (plan.name === "runtime migration") hint = "Check the database connection and migration error above. Verify Docker services with ./dev.sh status; do not reset a database to fix an unexplained migration failure.";
  else if (plan.name === "infra") hint = "Check Docker and the service output above. Inspect containers with ./dev.sh status and their logs with docker compose -f scripts/dev/docker-compose.yaml logs --tail=50.";
  else if (!tail.trim()) hint = "Run the displayed command in its working directory to inspect its failure, then retry the launcher.";
  return new DevError(`${cause ? "Could not launch" : "Command failed:"} ${plan.name}.`, { details, hint, cause });
}

export function signalCommand(record, signal) {
  if (!record.child.pid) return;
  try {
    if (process.platform === "win32") record.child.kill(signal);
    else process.kill(-record.child.pid, signal);
  } catch (error) {
    if (error.code !== "ESRCH") throw error;
  }
}

export async function stopCommands(records, { graceMs = 2_000, write = console.error } = {}) {
  const active = records.filter(commandAlive);
  for (const record of active) {
    try { signalCommand(record, "SIGTERM"); }
    catch (error) { write(redact(`[cleanup] Could not stop ${record.name}: ${error.message}`)); }
  }
  const deadline = Date.now() + graceMs;
  while (active.some(commandAlive) && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  for (const record of active) {
    if (!commandAlive(record)) continue;
    write(`[cleanup] ${record.name} did not exit after ${graceMs / 1_000}s; sending SIGKILL.`);
    try { signalCommand(record, "SIGKILL"); }
    catch (error) { write(redact(`[cleanup] Could not kill ${record.name} (pid ${record.child.pid}): ${error.message}`)); }
  }
  if (active.length) write(`[cleanup] Stop requested for: ${active.map((record) => record.name).join(", ")}.`);
}

function commandAlive(record) {
  if (!record.child.pid) return false;
  if (process.platform === "win32") return !record.closed;
  try {
    process.kill(-record.child.pid, 0);
    return true;
  } catch (error) {
    return error.code !== "ESRCH";
  }
}
