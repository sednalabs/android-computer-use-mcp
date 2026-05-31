# android-computer-use-mcp

`android-computer-use-mcp` is a local-first MCP server for Android emulator control,
observation, and artifact capture. It gives agents and harnesses a structured
way to launch emulators, install apps, inspect UI state, drive Android input, and
collect screenshots, UI hierarchies, logcat output, and scenario manifests.

The server is designed for trusted local or hosted-runner automation. It binds to
loopback, shells out only through configured Android SDK tools, writes artifacts
under a configured directory, and keeps the public MCP tool contract stable while
backend details such as ADB and emulator gRPC remain implementation details.

## What It Provides

- Emulator lifecycle tools for AVD discovery, launch, readiness, app install, and
  app launch.
- Observation tools for screenshots, UIAutomator XML, normalized UI summaries,
  stable-window waits, and logcat capture.
- Semantic UI tools for selector-based find, wait, tap, text entry, and scrolling
  with verified postconditions.
- Raw Android input tools for coordinate taps, text, swipes, and key events when
  semantic selectors are not enough.
- Hosted interactive-session helpers for runner-backed APK install and relaunch
  workflows.
- Optional domain scenario tools that demonstrate how app-specific flows can sit
  on top of the generic Android control surface.
- Adapter packages for Codex-style thread item packaging and standalone OpenAI
  Responses loops.

## Documentation

- [Getting Started](docs/GETTING_STARTED.md)
- [Tool Guide](docs/TOOL_GUIDE.md)
- [Security Model](docs/SECURITY_MODEL.md)
- [Codex MCP Wiring](docs/codex-mcp-wiring.md)
- [OpenAI Code-Execution Adapter](docs/openai-code-execution-adapter.md)

## Repository Layout

- `src/` contains the Rust MCP server, Android tool implementations, local HTTP
  runtime, resources, verification helpers, and tool inventory.
- `spec/` contains committed MCP schema and resource catalog snapshots.
- `adapters/codex/` packages Android observations as Codex-compatible raw
  Responses/thread items without making direct OpenAI API calls.
- `adapters/openai/` contains standalone OpenAI Responses helper utilities,
  Streamable HTTP MCP client helpers, file upload brokering, and a thin loop
  driver.
- `docs/examples/` contains executable JavaScript examples for runtime state,
  multimodal item packaging, computer-style action bridging, and optional
  scenario-specific review flows.
- `scripts/run_local.sh` starts the local loopback server with the environment
  you provide.

## Quick Start

Set the Android SDK paths for your machine and start the loopback server:

```bash
export ANDROID_COMPUTER_USE_MCP_SDK_ROOT="$HOME/Android/Sdk"
export ANDROID_COMPUTER_USE_MCP_ADB_PATH="$HOME/Android/Sdk/platform-tools/adb"
export ANDROID_COMPUTER_USE_MCP_EMULATOR_PATH="$HOME/Android/Sdk/emulator/emulator"
export ANDROID_COMPUTER_USE_MCP_AVDMANAGER_PATH="$HOME/Android/Sdk/cmdline-tools/latest/bin/avdmanager"
export ANDROID_COMPUTER_USE_MCP_ARTIFACT_DIR="./artifacts"
./scripts/run_local.sh
```

Check health:

```bash
curl http://127.0.0.1:9526/health
```

Inspect the registered tool surface:

```bash
cargo run -- --print-tools
```

For a fuller setup walkthrough, see [Getting Started](docs/GETTING_STARTED.md).

## Configuration

The server can be configured with CLI flags and environment variables. Important
environment variables include:

- `ANDROID_COMPUTER_USE_MCP_SDK_ROOT`
- `ANDROID_COMPUTER_USE_MCP_ADB_PATH`
- `ANDROID_COMPUTER_USE_MCP_EMULATOR_PATH`
- `ANDROID_COMPUTER_USE_MCP_AVDMANAGER_PATH`
- `ANDROID_COMPUTER_USE_MCP_ARTIFACT_DIR`
- `ANDROID_COMPUTER_USE_MCP_EMULATOR_GRPC_PORT`
- `ANDROID_COMPUTER_USE_MCP_USE_SG_KVM`
- `ANDROID_COMPUTER_USE_MCP_BIND_ADDR`
- `ANDROID_COMPUTER_USE_MCP_ALLOWED_HOSTS`
- `ANDROID_COMPUTER_USE_MCP_HTTP_MAX_SESSIONS`
- `ANDROID_COMPUTER_USE_MCP_HTTP_CHANNEL_CAPACITY`
- `ANDROID_COMPUTER_USE_MCP_HTTP_ALLOW_RESUME`
- `ANDROID_COMPUTER_USE_MCP_INTERACTIVE_SESSION_ROOT`
- `ANDROID_COMPUTER_USE_MCP_INTERACTIVE_SESSION_GITHUB_REPOSITORY`
- `ANDROID_COMPUTER_USE_MCP_INTERACTIVE_SESSION_APP_PACKAGE`
- `ANDROID_COMPUTER_USE_MCP_INTERACTIVE_SESSION_APP_ACTIVITY`
- `ANDROID_COMPUTER_USE_MCP_INTERACTIVE_SESSION_GITHUB_TOKEN`

The default HTTP bind address is `127.0.0.1:9526`. Non-loopback bind addresses
are rejected by configuration validation in this release line.

## Tool Surface

The committed schema snapshot currently includes these public tool groups:

- Core health and discovery: `android.health`, `android.list_avds`,
  `android.list_devices`
- Emulator lifecycle: `android.launch_avd`, `android.launch_avd_and_wait`,
  `android.wait_for_boot`, `android.install_apk`, `android.launch_app`
- Observation: `android.capture_screenshot`, `android.dump_ui_hierarchy`,
  `android.collect_logcat`, `android.inspect_ui`, `android.wait_for_stable_ui`
- Semantic UI: `android.find_ui_element`, `android.wait_for_ui_element`,
  `android.tap_element`, `android.type_into_element`,
  `android.scroll_until_visible`
- Raw input: `android.input.tap`, `android.input.text`, `android.input.swipe`,
  `android.input.keyevent`
- Hosted interactive session: `interactive_session.get_status`,
  `interactive_session.get_current_build`,
  `interactive_session.install_build_from_run`,
  `interactive_session.relaunch_current_build`
- Optional app-specific scenarios: `solarlab.scenario.stage_first_focus_earth`,
  `solarlab.scenario.stage_first_immersive_roundtrip`,
  `solarlab.semantic_action`

The app-specific scenario tools are examples of the scenario pattern. Generic
Android automation should start with the `android.*` and `interactive_session.*`
tools and load scenario-specific tools only when the target app requires them.

See [Tool Guide](docs/TOOL_GUIDE.md) for usage order, postcondition behavior,
and adapter guidance.

## Backend Notes

ADB is the stable baseline backend. The server can opportunistically use the
Android emulator gRPC endpoint for screenshot and raw input operations when a
usable endpoint is discovered or configured:

- if a running emulator publishes `grpc.port`, the MCP can prefer that endpoint
- if a `grpc.token` is published, the MCP attaches the required bearer token
- if `ANDROID_COMPUTER_USE_MCP_EMULATOR_GRPC_PORT` is set, launched emulators include
  `-grpc <port>`
- if gRPC is unavailable or unhealthy, the MCP falls back to ADB

This keeps the tool contract stable while allowing lower-latency local emulator
control where it is available.

## Adapter Packages

The Codex adapter in `adapters/codex/` produces Codex-compatible raw Responses
items and `thread/inject_items` payloads. It does not make direct OpenAI API
calls and does not require `OPENAI_API_KEY`.

The OpenAI adapter in `adapters/openai/` is a standalone helper layer for direct
Responses API loops. It packages screenshots as `input_image` content and XML,
logs, and manifests as `input_file` content, with optional file upload brokering.

Both adapter packages keep model-facing multimodal items separate from the Rust
MCP server so the server can remain focused on Android lifecycle, artifact
ownership, and truthful tool results.

## Validation

Useful local checks include:

```bash
cargo test
cargo fmt --all
cargo clippy --all-targets --all-features
```

The tool schema snapshot is checked by the Rust test suite. Refresh it only when
an intentional public tool contract change has been made:

```bash
MCP_TOOLKIT_UPDATE_TOOL_SNAPSHOTS=1 cargo test tool_schema_snapshot_contract_is_stable
```

For documentation-only changes, compile-free checks such as `git diff --check`,
Markdown link checks, and public wording scans are usually sufficient.

## Security

Read [Security Model](docs/SECURITY_MODEL.md) before exposing this server to any
automation runner. The short version:

- keep the HTTP server on loopback
- use a dedicated artifact directory
- pass only trusted APKs and configured Android SDK tools
- treat screenshots, UI XML, logcat, and scenario manifests as potentially
  sensitive
- avoid putting secrets in prompts, tool arguments, logs, or committed examples

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).
