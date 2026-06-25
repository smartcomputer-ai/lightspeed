# Example Profiles

These files are importable profile documents for local development and demos.

Import the workspace-backed profile:

```bash
cargo run -p cli -- profiles import profiles/workspace-prompts-skills.json
```

That import uploads `profiles/workspace-prompts-skills/` as a VFS workspace and
mounts it at `/workspace`. The `provision` block is consumed locally by the CLI
and is not stored in the profile record.

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
