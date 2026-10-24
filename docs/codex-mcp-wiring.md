# Codex MCP Wiring

This server is ready to be wired into Codex as a local loopback Streamable HTTP MCP.

## Recommended Codex config snippet

Add a block like this to `~/.codex/config.toml`:

```toml
[mcp_servers.android_computer_use_mcp]
url = "http://127.0.0.1:9526/mcp"
tool_timeout_sec = 300.0
enabled = false
default_tools_approval_mode = "prompt"
enabled_tools = [
  "android.health",
  "android.list_devices",
  "android.wait_for_boot",
  "android.capture_screenshot",
  "android.inspect_ui",
  "android.wait_for_stable_ui",
  "android.find_ui_element",
  "android.wait_for_ui_element",
  "android.tap_element",
  "android.type_into_element",
  "android.input.keyevent",
  "android.input.swipe",
  "android.input.tap",
  "android.input.text",
  "android.scroll_until_visible",
  "interactive_session.get_status",
  "interactive_session.get_current_build",
  "interactive_session.install_build_from_run",
]
```

Keep `enabled = false` until the operator is ready to turn the server on in
their active Codex profile. The allow-list covers a hosted interactive session's
readiness, visual/selector-driven interaction, and build provenance/install
flow; add diagnostics or raw input tools only when the use case needs them.
Prompt approval is retained for observation, input, and build installation while
qualifying the exact endpoint and session.

For a hosted session, change only the URL to the exact MCP endpoint supplied by
that session and use its existing access policy. Do not put access tokens in the
Codex configuration file or public workflow inputs.

## Model-visible screenshots

`android.capture_screenshot` returns the PNG as an MCP image content item and
keeps the serial, backend, and artifact path in a JSON text receipt. Image-bearing
results omit `structuredContent`: Codex prefers that field when present and does
not forward accompanying content blocks to the model. Results without an image
continue to use structured content.
`android.inspect_ui` and `android.wait_for_stable_ui` attach the same image item
when screenshot capture is enabled. Codex can use these namespaced tools through
its generic MCP configuration; no Codex-specific dynamic-tool adapter is
required. The artifact paths and hierarchy remain evidence for replay or
debugging, while the typed image is the model-visible visual observation.

Start the daemon separately from the repository checkout:

```bash
cd "<checkout>/android-computer-use-mcp"
ANDROID_COMPUTER_USE_MCP_SDK_ROOT="$HOME/Android/Sdk" \
ANDROID_COMPUTER_USE_MCP_ADB_PATH="$HOME/Android/Sdk/platform-tools/adb" \
ANDROID_COMPUTER_USE_MCP_EMULATOR_PATH="$HOME/Android/Sdk/emulator/emulator" \
ANDROID_COMPUTER_USE_MCP_AVDMANAGER_PATH="$HOME/Android/Sdk/cmdline-tools/latest/bin/avdmanager" \
ANDROID_COMPUTER_USE_MCP_ARTIFACT_DIR="$PWD/artifacts" \
ANDROID_COMPUTER_USE_MCP_USE_SG_KVM=0 \
RUST_LOG=info \
./scripts/run_local.sh
```

## What the server expects

- Android SDK installed at the configured path
- `adb`, `emulator`, and `avdmanager` available at the configured file paths
- an artifact directory the server can write into
- KVM group access when launching an x86_64 emulator locally

The server validates the configured binary paths at startup and exits early with
an actionable error if one is missing.

## What `ffmpeg` is for

`ffmpeg` is not required for the MCP server handshake or the ADB-first tool
surface.

It is useful for adjacent ergonomics work:

- extracting frames from operator screen recordings
- building before/after visual evidence bundles
- supporting future video-based artifact helpers outside the core MCP surface

## Current architecture note

The current server uses a small Rust loopback Streamable HTTP MCP based on
`rmcp`, because that keeps the transport restartable without making Codex
responsible for spawning a fresh child server for each session.

This server uses a narrow `mcp-toolkit-rs` seam for schema snapshots and bounded
local session bootstrap.

That toolkit is a good candidate for selective future hardening once we want:

- tool-schema contract enforcement
- broader tool inventory composition
- shared observability or policy surfaces across servers

It is not a prerequisite for every harness feature, and the server should not be
rewritten around toolkit abstractions unless that clearly reduces risk. The
current loopback runtime uses only a small toolkit seam for bounded local
session bootstrap.

## Deferred tool-loading note

For large Android sessions, do not assume every Android tool must be eagerly
surfaced before the session starts.

The current runtime examples now treat the surface in layers:

- eager bootstrap tools for discovery and app entry
- deferred observation tools
- deferred semantic UI tools
- deferred raw input tools
- deferred app-specific scenario tools

If the client supports a tool-search or deferred-loading hook, prefer using it
for those deferred groups instead of forcing the whole Android schema set into
the initial session context.
