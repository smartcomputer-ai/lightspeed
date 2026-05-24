# eval

Prompt-level eval harness for Forge agent workflows.

## Commands

- `cargo run -p eval -- list`
- `cargo run -p eval -- case read-file`
- `cargo run -p eval -- all --runs 3`

`case` and `all` execute live OpenAI Responses API calls and require
`OPENAI_API_KEY`. `OPENAI_BASE_URL`, `OPENAI_ORG_ID`, and `OPENAI_PROJECT_ID`
are also honored when present.

Each attempt gets a fresh temporary workspace, seeded files, the `test-support`
runner harness, and an inline host tool executor. Assertions cover tool calls,
tool output text, final assistant text, and workspace file state.
