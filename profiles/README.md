# Example Profiles

These files are importable agent profile documents for local development and demos.
The CLI accepts either one profile object per file or a non-empty JSON array of
profile objects for batch import/check.

Import the workspace-backed profile:

```bash
cargo run -p cli -- profiles import profiles/workspace-prompts-skills.json
```

That import uploads `profiles/workspace-prompts-skills/` as a VFS workspace and
mounts it at `/workspace`. The `provision` block is consumed locally by the CLI
and is not stored in the profile record.

Import the fleet demo profile set:

```bash
cargo run -p cli -- profiles import profiles/fleet-demo.json
cargo run -p cli -- chat --new --profile example.fleet.supervisor
```

The supervisor profile enables Fleet tools and routes work to three named child
profiles. The child profiles use different prompt instructions and model
configuration: OpenAI children use `providerId: "openai"` with
`apiKind: "openai:responses"`, while the reviewer uses
`providerId: "anthropic"` with `apiKind: "anthropic:messages"`.

Register the public MCP echo test server before importing the MCP profile:

```bash
cargo run -p cli -- mcp server add \
  --api-url http://127.0.0.1:18080/rpc \
  --id echo-playground \
  --label echo \
  --display-name "MCP Echo Playground" \
  --description "Public echo server for MCP smoke tests." \
  https://mcpplaygroundonline.com/mcp-echo-server
```

Then import the MCP profile:

```bash
cargo run -p cli -- profiles import profiles/mcp-echo.json
```

Profile imports validate referenced mounts, MCP servers, and environments by
default. Use `--no-check` only when you want to store the profile before the
referenced resources exist.
