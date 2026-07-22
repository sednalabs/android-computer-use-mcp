# OpenAI Adapter Helpers

This package is the supported standalone OpenAI adapter layer for
`android-computer-use-mcp`.

It keeps the MCP server as the Android control plane and exposes a small set of
OpenAI-oriented helpers for:

- persistent Android runtime state
- Solar Lab review sessions
- native Responses multimodal items
- native `function_call_output` and `computer_call_output` envelopes
- streamable HTTP MCP client helpers for live hosted sessions
- OpenAI file-upload brokering for `file_id`-backed image and file items
- a thin Responses loop driver for GPT-5.4-style Android observation

The important boundary is:

- MCP tools return truthful structured Android results plus artifact paths
- this adapter packages those artifacts into model-native image and file items

## Why this exists

GPT-5.4 can reason natively over screenshots returned from a tool loop, but the
documented path is through Responses items such as:

- `input_image`
- `input_file`
- `function_call_output`
- `computer_call_output`

This package gives the Android harness a first-class place to construct those
items without pushing OpenAI-specific response-schema concerns down into the
Rust MCP server.

It is not the default Codex CLI control plane.

For Codex-first use:

- keep Codex as the thing that reasons and drives the loop
- keep `android-computer-use-mcp` as the Android control plane
- prefer `adapters/codex/` when you want Codex-ready items without a separate
  OpenAI API credential

## Current entrypoints

- `createAndroidEmulatorRuntime`
- `createAndroidExecutionRun`
- `createAndroidComputerBridge`
- `createSolarLabHelpers`
- `createSolarLabReviewSession`
- `createAndroidResponsesItemAdapter`
- `createFunctionCallOutputItem`
- `createComputerCallOutputItem`
- `createMcpStreamableHttpClient`
- `createOpenAiFileBroker`
- `createOpenAiResponsesLoopDriver`

The package also ships a small CLI:

- `bin/openai-android-loop.mjs`

That CLI is designed for hosted interactive-session use when you explicitly want
the runner itself to make direct OpenAI Responses API calls:

- it talks to a live `android-computer-use-mcp` Streamable HTTP endpoint
- it uses the native Responses loop instead of raw MCP-only observation
- it prefers OpenAI `file_id` uploads for screenshots and XML/log artifacts when
  `OPENAI_API_KEY` is available

## Screenshot fidelity default

Screenshot-return helpers default to `detail: "original"` so GPT-5.4 sees the
full-resolution image unless a caller deliberately lowers fidelity. When the
host enables upload brokering, image items can also prefer uploaded `file_id`
references instead of inline data URLs.
