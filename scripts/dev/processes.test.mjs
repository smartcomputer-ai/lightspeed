import assert from "node:assert/strict";
import { once } from "node:events";
import test from "node:test";
import { launchCommand, stopCommands } from "./processes.mjs";
import { formatFailure } from "./errors.mjs";

const plan = (code) => ({ name: "fixture", command: process.execPath, args: ["-e", code], cwd: process.cwd(), env: process.env });

test("a missing executable explains what failed and how to check it", async () => {
  const command = { ...plan(""), command: "/missing/lightspeed-test-command" };
  const record = launchCommand(command, { write() {} });
  await assert.rejects(record.done, (error) => {
    const message = formatFailure(error);
    assert.match(message, /Could not launch fixture/);
    assert.match(message, /ENOENT/);
    assert.match(message, /working directory exists/);
    assert.doesNotMatch(message, /at ChildProcess/);
    return true;
  });
});

test("failed commands retain bounded output, command, directory, and exit status", async () => {
  const record = launchCommand(plan('for(let i=0;i<100;i++) console.error(`line ${i}`); process.stderr.write("compiler failure"); process.exitCode=7;'), { write() {} });
  await assert.rejects(record.done, (error) => {
    const message = formatFailure(error);
    assert.match(message, /Exited with code 7/);
    assert.match(message, /Directory:/);
    assert.match(message, /compiler failure/);
    assert.match(message, /  line 99/);
    assert.doesNotMatch(message, /  line 0\n/);
    return true;
  });
});

test("credential bootstrap stdout never enters logs or failure summaries", async () => {
  let printed = "";
  const command = plan('console.log(process.env.FIXTURE_SECRET); console.error("bootstrap failed"); process.exitCode=1;');
  command.env = { ...process.env, FIXTURE_SECRET: "SENSITIVE_CREDENTIAL" };
  const record = launchCommand(command, {
    captureStdout: true, write(text) { printed += text; },
  });
  await assert.rejects(record.done, (error) => {
    assert.doesNotMatch(printed + formatFailure(error, { debug: true }), /SENSITIVE_CREDENTIAL/);
    assert.match(printed, /bootstrap failed/);
    return true;
  });
});

test("shutdown stops siblings and escalates a process that ignores SIGTERM", { timeout: 5_000 }, async (t) => {
  const records = [
    launchCommand(plan('setInterval(()=>{}, 1000); console.log("ready");'), { write() {} }),
    launchCommand(plan('process.on("SIGTERM",()=>{}); setInterval(()=>{}, 1000); console.log("ready");'), { write() {} }),
  ];
  const outcomes = Promise.allSettled(records.map((record) => record.done));
  t.after(() => stopCommands(records, { graceMs: 50, write() {} }));
  await Promise.all(records.map((record) => once(record.child.stdout, "data")));
  const messages = [];
  await stopCommands(records, { graceMs: 50, write(text) { messages.push(text); } });
  await outcomes;
  assert.ok(records.every((record) => record.closed));
  assert.match(messages.join("\n"), /sending SIGKILL/);
});

test("summaries redact URL credentials and include stacks only in debug mode", () => {
  const error = new Error("Failed at postgres://alice:password@localhost/db?token=secret");
  const summary = formatFailure(error);
  assert.doesNotMatch(summary, /alice|password|secret|at TestContext/);
  assert.match(formatFailure(error, { debug: true }), /at TestContext/);
});

test("a crashed watcher is reported even if its child keeps output pipes open", { timeout: 5_000 }, async (t) => {
  const childCode = 'process.on("SIGTERM",()=>{console.log("descendant stopped");process.exit(0)});setInterval(()=>{},1000);process.send("ready");';
  const code = `const {spawn}=require("node:child_process");
    const child=spawn(process.execPath,["-e",${JSON.stringify(childCode)}],{stdio:["ignore","inherit","inherit","ipc"]});
    child.on("message",()=>{console.error("watcher crashed");process.exit(7)});`;
  let output = "";
  const record = launchCommand(plan(code), { write(text) { output += text; } });
  t.after(() => stopCommands([record], { graceMs: 50, write() {} }));
  await assert.rejects(record.done, /Command failed: fixture/);
  await stopCommands([record], { graceMs: 200, write() {} });
  assert.match(output, /descendant stopped/);
});
