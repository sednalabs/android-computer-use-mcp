# Getting Started

This guide starts a local `android-computer-use-mcp` server and connects it to a
loopback Android emulator automation workflow.

## Prerequisites

- Rust toolchain with the edition used by this crate.
- Android SDK with `adb`, `emulator`, and `avdmanager`.
- At least one Android Virtual Device if you want the MCP to launch an emulator.
- A trusted local shell. This server is intended for local or runner-owned
  automation, not arbitrary remote callers.

## Configure Paths

Set the required Android SDK paths in the shell that starts the server:

```bash
export ANDROID_COMPUTER_USE_MCP_SDK_ROOT="$HOME/Android/Sdk"
export ANDROID_COMPUTER_USE_MCP_ADB_PATH="$HOME/Android/Sdk/platform-tools/adb"
export ANDROID_COMPUTER_USE_MCP_EMULATOR_PATH="$HOME/Android/Sdk/emulator/emulator"
export ANDROID_COMPUTER_USE_MCP_AVDMANAGER_PATH="$HOME/Android/Sdk/cmdline-tools/latest/bin/avdmanager"
export ANDROID_COMPUTER_USE_MCP_ARTIFACT_DIR="./artifacts"
export ANDROID_COMPUTER_USE_MCP_BIND_ADDR="127.0.0.1:9526"
```

The server validates configured binary paths at startup and exits with an
actionable error if one is missing.

## Start the Server

```bash
./scripts/run_local.sh
```

The default Streamable HTTP MCP endpoint is:

```text
http://127.0.0.1:9526/mcp
```

Check the health endpoint:

```bash
curl http://127.0.0.1:9526/health
```

List registered tools:

```bash
cargo run -- --print-tools
```

## First Tool Flow

Start with the generic Android tools:

1. `android.health`
2. `android.list_devices`
3. `android.list_avds` if no device is already running
4. `android.launch_avd_and_wait` to launch and wait for boot
5. `android.install_apk` for the target APK
6. `android.launch_app` for the target package/activity
7. `android.wait_for_stable_ui` before acting on the UI
8. `android.tap_element` or `android.type_into_element` for semantic interaction
9. `android.capture_screenshot`, `android.dump_ui_hierarchy`, or
   `android.collect_logcat` for proof artifacts

Prefer semantic UI tools when a selector can describe the target. Use
`android.input.*` tools as explicit fallbacks for coordinate or key-event work.

## Codex Wiring

See [Codex MCP Wiring](codex-mcp-wiring.md) for a `~/.codex/config.toml`
snippet. Keep the server disabled in your active Codex profile until you are
ready to route Android work through it.

## OpenAI Responses Wiring

Use `adapters/openai/` when a standalone runner should call the OpenAI Responses
API directly. That adapter connects to the live Streamable HTTP MCP endpoint,
packages screenshots as image items, and can upload XML/log/manifest artifacts
as file items when a host-side broker is configured.

Use `adapters/codex/` when Codex is already the reasoning loop and you only need
Codex-compatible thread items.

## Optional Scenario Tools

The repository includes app-specific scenario tools as an example of how a
domain flow can be layered on the generic Android tool surface. Treat those
tools as optional and load them only when the target app and task require that
specific domain behavior.

Run a built-in scenario directly only when you intentionally want that app-level
flow:

```bash
cargo run -- --run-scenario stage_first_focus_earth --serial emulator-5554 --package-name <android.package.name>
```

## Troubleshooting

- If startup fails, check that the configured SDK paths point to real files.
- If a launched emulator is slow to become ready, use `android.wait_for_boot`
  before installing or launching an app.
- If semantic selectors are ambiguous, add `match_index` or a more specific
  selector instead of accepting an arbitrary first match.
- If screenshots or XML are missing, verify that the artifact directory exists
  and is writable by the server process.
- If a hosted runner cannot resume a session, check
  `ANDROID_COMPUTER_USE_MCP_HTTP_ALLOW_RESUME` and the configured session limits.
