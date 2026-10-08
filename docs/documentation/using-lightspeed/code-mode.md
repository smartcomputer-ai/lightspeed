# Code mode

Code mode lets the model compose its tools with JavaScript. It can call several
tools, loop over results, run independent work in parallel, and pass one tool's
output into another—all within one `code_execute` call.

This is useful when a task has a clear procedure: collect records from an MCP
service, filter and aggregate them, save a report, or coordinate several jobs.
The script handles the intermediate steps and selects what to show the model.
That can reduce model round trips and keep large intermediate results out of
its conversation context.

## Enable code mode

Enable **Code mode** in a profile or in an idle session's **Session settings**.
Keep the other capabilities the task needs enabled too: code mode composes
the session's available tools. Ordinary tool calls remain available alongside
`code_execute`, so the model can choose either approach.

For API configuration, the feature can use defaults:

```json
{
  "features": {
    "codeMode": {}
  }
}
```

Under **Customize limits**, you can adjust:

| Setting | Default | Meaning |
| --- | --- | --- |
| Timeout | 60,000 ms | Total time for one script, including tool calls and waits. |
| Max tool calls | 128 | Total calls the script can make. |
| Max outstanding calls | 16 | Pending tool calls allowed at once. |

The JSON fields are `timeoutMs`, `maxToolCalls`, and
`maxOutstandingToolCalls`. A script call can request a shorter `timeout_ms`;
it cannot exceed the configured timeout. The maximum configurable timeout is
600,000 ms. By default, all otherwise callable session tools are available;
advanced configuration can narrow them with `allowedTools` using logical tool
IDs such as `vfs.read_file`.

## Compose tools in a script

The model supplies the body of an async JavaScript function as the `code`
argument to `code_execute`. It can use `await`, loops, conditionals,
`Promise.all`, and `Promise.allSettled`. Each `tools.tool_name(arguments)` call
returns a JavaScript promise for that tool's result.

For example, suppose an attached workspace contains two JSON reports, each
with numeric `revenue` and `cost` fields. A script can read both concurrently,
calculate profit, and produce a downloadable summary:

```js
const paths = [
  "/workspace/reports/january.json",
  "/workspace/reports/february.json",
];

const reports = await Promise.all(paths.map(async (path) => {
  const result = await tools.vfs_read_file({ path });
  const report = JSON.parse(result.text);
  return { path, profit: report.revenue - report.cost };
}));

const totalProfit = reports.reduce((sum, report) => sum + report.profit, 0);
text({ totalProfit, unprofitable: reports.filter((report) => report.profit < 0) });
await file({ json: reports }, { name: "profit-report.json" });
```

The model writes these scripts from the tool definitions it sees. Names and
arguments follow the selected model's tool format; for example, Anthropic's
VFS read tool is `VfsRead`. Available output JSON Schemas are included in tool
descriptions to help the model process results correctly.

For MCP, use **Lightspeed connects**. Scripts can call tools exposed up front,
or use `mcp_find_tools` and `mcp_call` for search-on-demand servers. Jobs and
sub-agents work through the same tools as ordinary calls: `job_run` and
`agent_run` wait for their results, while submitted work can return handles
for later use with `tools.await(...)`.

## Select the output

Intermediate tool results are available to the script without automatically
becoming output for the model. Use these helpers to select what it receives:

| Helper | Purpose |
| --- | --- |
| `text(value)` | Emit a string or JSON value, such as selected records or a summary. |
| `media(source)` | Show a supported image or PDF to the model. Await this helper. |
| `file(source, options)` | Publish a downloadable file attachment. Await this helper. |
| `return value` | Supply a final JSON value separately from emitted output. |

`media()` and `file()` accept content descriptors, full `sha256:` references,
and recorded `media:` or `file:` handles. They also accept inline objects such
as `{ text: "..." }`, `{ json: value }`, or `{ bytes: [...] }`. A string means
a content reference, so use an explicit `{ text: "..." }` object for literal
text. These helpers use the ordinary blob tools and count toward tool limits.

To show an image read from a workspace:

```js
const image = await tools.vfs_read_file({ path: "/workspace/chart.png" });
await media(image.media);
```

Use workspace tools to save working files across calls, and blob references
to pass stored content between tools. JavaScript variables are local to each
execution.

## Follow execution and failures

Lightspeed runs the script in its own workflow and routes its tool calls back
through the session. Those calls use the existing tool execution machinery;
individual effects retain their configured retry behavior. The transcript
shows **Run code**, its source line count, and tool activity. Expand the call
to inspect the script and result.

A failed tool call rejects its JavaScript promise. The script can handle it
with `try`/`catch` or `Promise.allSettled`. If the script fails, the result
reports the error and completed tool outcomes, retaining earlier selected
output when available. Completed effects are not rolled back, and Lightspeed
does not automatically rerun the whole script. The model can inspect the
report and decide how to continue. Await work before finishing the script;
unawaited calls are cancelled when it ends.
