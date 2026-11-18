# Getting Started

This guide starts a local `android-computer-use-mcp` server and connects it to a
loopback Android emulator automation workflow.

## Prerequisites

- Rust 1.99 or newer and the Protobuf compiler (`protoc`) on `PATH`.
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
cargo run --locked -- --print-tools
```

## First Tool Flow

Start with `android.health` and `android.list_devices`. If an emulator needs to
be started, use `android.list_avds` and `android.launch_avd_and_wait`.

Resolve the exact device with `android.resolve_target`, then use
`android.observe` for a screenshot and compact UI receipt. Send semantic actions
through `android.step`, assigning a unique action ID to each intended batch.
The server executes the batch once, stops after failure and returns a fresh
observation. After a lost response, recover the existing ID with
`android.get_action_status` before deciding the next action.

For a configured hosted build, use `android.install_build_from_run` with the
exact build identity. `android.install_apk` remains available for a local APK.
Prefer semantic selectors when they describe the target; coordinate actions
require a recent observation ID. See [Agent Workflows and Recovery](agent-workflows.md)
for complete requests, outcome interpretation and deployment limits.

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
