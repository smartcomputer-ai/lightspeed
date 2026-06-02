# Basic Skill Fixture

Mount this directory in chat to exercise skill discovery, the TUI picker, and
direct skill activation.

```bash
cargo run -p worker
cargo run -p gateway
cargo run -p cli -- chat --new --api-url http://127.0.0.1:18080/rpc --mount dev/skill-fixtures/basic
```

Inside the TUI:

```text
/skills
/skill
/skills-active
```

The `/skill` command opens a picker. Select the matrix migration skill, then ask:

```text
What is the matrix migration marker?
```

Expected marker if the right skill is loaded:

```text
MATRIX-MARKER=RUNE-ORBIT-4179
```

The generated skill ids are content/location-derived, so use `/skills` or the
picker instead of assuming the directory names are the ids.
