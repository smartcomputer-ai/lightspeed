# agent-eval

Prompt-level eval harness for the Forge local agent runtime.

## Commands

- `cargo run -p agent-eval -- list`
- `cargo run -p agent-eval -- case read-file`
- `cargo run -p agent-eval -- all --runs 3`

`case` and `all` execute live OpenAI Responses API calls and require
`OPENAI_API_KEY`. `OPENAI_BASE_URL`, `OPENAI_ORG_ID`, and `OPENAI_PROJECT_ID`
are also honored when present.

Each attempt gets a fresh temporary workspace, seeded files, a process-local
`LocalAgentApi`, and an inline host tool executor. Assertions cover tool calls,
tool output text, final assistant text, and workspace file state.
