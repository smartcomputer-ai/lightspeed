# Codex-like editing tools

**Status:** Implemented and verified offline, 2026-10-29.

The editing presentation determines tool preference; the provider adapter owns
the wire format. No model-name heuristics or public configuration fields are
needed.

- Responses defaults and explicit `codex_like` presentations prefer patches and
  omit exact-replacement editing. Whole-file writes remain available for binary
  content, blob transfers, and full replacements.
- Responses sends Codex-like patches as custom freeform tools using the patch
  grammar. Chat Completions sends explicitly selected Codex-like patches as JSON
  functions. Generic Completions defaults retain exact replacement and omit patches.
- Canonical presentation retains JSON patching and exact replacement.
- Script callables keep the JSON `patch` argument and existing execution binding.
  Direct freeform input is normalized at the Responses adapter boundary.
- Native calls are retained unchanged. Replay and compaction select output item
  types from the original calls, so mixed JSON/freeform history survives changes
  of presentation. Already-admitted exact-replacement calls remain executable.

Validation passed for the affected crate unit suites, provider catalogs, forced
tool choice, script schemas, patch application, mixed-format replay, compaction,
truncated output, reused call IDs, and historical execution bindings. Full
workspace Clippy with all targets and warnings denied, formatting, and the
documentation checks also passed. Live model quality comparisons were not run.
