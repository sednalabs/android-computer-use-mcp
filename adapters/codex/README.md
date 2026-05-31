# Codex Bridge Helpers

This package is the Codex-facing companion to `android-computer-use-mcp`.

It is intentionally different from the standalone OpenAI Responses helper:

- it does not make direct OpenAI API calls
- it does not require `OPENAI_API_KEY`
- it produces Codex-ready raw Responses items and `thread/inject_items`
  payloads that can be appended to a Codex thread when a thread transport handle
  is available

Current entrypoints:

- `createCodexThreadItemsAdapter`
- `createMessageItem`
- `createThreadInjectItemsParams`
- `contextFromObservation`

The package also ships a CLI:

- `bin/codex-android-observe.mjs`

That CLI talks to a live `android-computer-use-mcp` Streamable HTTP endpoint,
captures a fresh Android observation, and emits either:

- a raw Codex-compatible `message` item
- or a ready-to-send `thread/inject_items` payload

Current limitation:

- upstream Codex already supports `input_image` in raw thread items and
  `codex.emitImage(...)` inside `js_repl`
- but ordinary Codex CLI sessions do not currently expose the active thread
  transport handle automatically to arbitrary runner-local scripts

So this package is the honest first bridge:

- Codex-native item packaging with no API key
- ready for `thread/inject_items` when a thread id and app-server transport are
  available
- no fake promise that a standalone shell script can silently mutate the active
  Codex conversation without that handle
