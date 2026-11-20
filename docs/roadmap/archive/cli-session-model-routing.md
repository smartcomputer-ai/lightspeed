# CLI session model routing

Status: implemented and validated, 2026-09-28.

The contributor's CLI improvements restore transcripts, session navigation,
usage statistics and provider discovery. Maintainer follow-up keeps those
changes while enforcing the session's provider-native context boundary.

- [x] Keep the contributor's original commits and merge current main into the
  existing branch before adding follow-up commits.
- [x] Pin configured provider identity and API kind from session creation;
  allow model changes and reversals within that route. Aggregator routes do
  not additionally constrain the underlying model vendor.
- [x] Enforce the rule in engine configuration admission/replay and run
  overrides, with early gateway validation and matching CLI/web controls.
- [x] Rotate bot sessions when a profile changes provider identity or API kind.
- [x] Clear the CLI's model override when switching to another session; reject
  incompatible startup flags and block model edits during pending submissions.
- [x] Surface transcript read failures, retain summary views, retry with
  `/refresh`, and fail one-shot/JSON commands if the transcript stays incomplete.
- [x] Update session guidance and regenerate public API descriptions and their
  consumers. No wire fields or durable event shapes change.

Validation covers CLI mock HTTP exchanges, picker behavior, engine replay,
gateway run overrides and web model editing. Credentialed suites are outside
this change's validation scope.

Validation results:

- CLI: 164 tests passed, including local HTTP session and transcript regressions.
- Engine: 230 tests passed with contract features, including configuration replay.
- API: 93 unit tests and seven contract artifact tests passed.
- Server library: 349 tests passed; the existing ffmpeg smoke test stayed ignored.
- Web: all 524 tests passed, with TypeScript checking also passing.
- CLI/engine Clippy passed with warnings denied; Rust formatting and diff
  whitespace checks passed. Public API artifacts and consumers were regenerated.
