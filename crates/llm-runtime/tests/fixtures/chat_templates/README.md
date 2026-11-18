# Chat templates of open models

The Jinja chat templates that self-hosted servers (vLLM, SGLang, llama.cpp
with `--jinja`) apply to Chat Completions requests. The conformance tests in
`src/openai_completions.rs` (`chat_templates`) render real request shapes
through them, so a request a template rejects, or a request whose rendered
prefix changes between turns, fails without a GPU.

Each file is the `chat_template` of the model's `tokenizer_config.json` (or its
`chat_template.jinja`) at the pinned revision, unchanged.

| File | Model | Revision | License |
| --- | --- | --- | --- |
| `qwen3.5.jinja` | `Qwen/Qwen3.5-9B` | `c202236235762e1c871ad0ccb60c8ee5ba337b9a` | Apache-2.0 |
| `qwen3.jinja` | `Qwen/Qwen3-8B` | `b968826d9c46dd6066d109eabc6255188de91218` | Apache-2.0 |
| `qwen2.5.jinja` | `Qwen/Qwen2.5-7B-Instruct` | `a09a35458c702b33eeacc393d103063234e8bc28` | Apache-2.0 |
| `gpt-oss.jinja` | `openai/gpt-oss-20b` | `6cee5e81ee83917806bbde320786a8fb61efebee` | Apache-2.0 |
| `mistral-v0.3.jinja` | `mistralai/Mistral-7B-Instruct-v0.3` | `c170c708c41dac9275d15a8fff4eca08d52bab71` | Apache-2.0 |

Llama and Gemma templates are left out: those models are gated behind their
own licenses. Templates without tool calling (Phi-4) are left out too.
