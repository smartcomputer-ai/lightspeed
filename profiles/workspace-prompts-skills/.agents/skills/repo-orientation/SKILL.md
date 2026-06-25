---
name: repo-orientation
description: Use when entering an unfamiliar repository or preparing a handoff that needs a concise map of relevant files, commands, and risks.
short_description: Map a repository quickly.
---

# Repo Orientation

Start by reading the repository index files, then inspect only the code paths
that matter for the current request.

Use fast search first:

```bash
rg --files
rg -n "keyword"
```

Capture the useful context in a short handoff:

- Relevant files and ownership boundaries
- Commands that verify the change
- Known risks or assumptions
