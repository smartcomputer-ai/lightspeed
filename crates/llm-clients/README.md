# llm-clients

Provider-native LLM API client primitives for Lightspeed.

This crate is being rebuilt around native provider API kinds:

- `openai:responses` in `openai::responses`
- `openai:completions` in `openai::completions`
- `anthropic:messages` in `anthropic::messages`

The crate does not provide a unified LLM message abstraction, provider registry,
global default client, agent loop, tool dispatcher, or CLI subprocess agent
backend.

## Current Status

P45.1 transport is implemented:

- HTTP client wrapper and URL helpers
- header snapshots and `Retry-After` parsing
- incremental SSE parser
- API response and stream event transport envelopes
- provider HTTP failure classification

P45.2 `openai:responses` is implemented:

- native request/response/stream records
- `Client::create`
- `Client::stream`
- `Client::retrieve`
- `Client::retrieve_stream`
- `Client::delete`
- `Client::cancel`
- `Client::compact`
- `Client::list_input_items`
- `Client::count_input_tokens`
- raw response JSON and raw SSE retention
- usage, output text, reasoning summary, function-call, and stream terminal
  helpers
- ignored live tests for text, streaming, retrieve, list input items, input
  token counts, delete, cancel, compact, function calls, usage, and provider
  error classification

P45.3 `openai:completions` is implemented:

- native chat completions request/response/stream records
- `Client::create`
- `Client::stream`
- raw response JSON and raw SSE retention
- usage, output text, tool-call, stream text-delta, and terminal chunk helpers
- ignored live tests for text, streaming, function calls, usage, and provider
  error classification

P45.4 `anthropic:messages` is implemented:

- native Messages request/response/stream records
- `Client::create`
- `Client::stream`
- `Client::count_tokens`
- `anthropic-version`, `anthropic-beta`, auth, base URL, and timeout wiring
- raw response JSON and raw SSE retention
- usage, output text, thinking block, `tool_use`, stream text-delta, and
  terminal event helpers
- ignored live tests for text, streaming, tool use, thinking, token counting,
  usage, and provider error classification

## Build

```bash
cargo test -p llm-clients
```
