# Codex MCP Wiring

This server is ready to be wired into Codex as a local loopback Streamable HTTP MCP.

## Recommended Codex config snippet

Add a block like this to `~/.codex/config.toml`:

```toml
[mcp_servers.android_computer_use_mcp]
url = "http://127.0.0.1:9526/mcp"
tool_timeout_sec = 420.0
enabled = false
default_tools_approval_mode = "prompt"
enabled_tools = [
  "android.health",
  "android.list_devices",
  "android.resolve_target",
  "android.wait_for_boot",
  "android.observe",
  "android.step",
  "android.get_action_status",
  "android.install_build_from_run",
  "interactive_session.get_status",
  "interactive_session.get_current_build",
]
```

Keep `enabled = false` until the operator is ready to turn the server on in
their active Codex profile. The allow-list covers a hosted interactive session's
readiness, visual/selector-driven interaction, and build provenance/install
flow; add diagnostics or raw input tools only when the use case needs them.
The example retains prompt approval while qualifying the endpoint. The primary
observation and action-status tools are annotated read-only; mutation and build
installation remain distinct policy decisions. See [Agent workflows](agent-workflows.md)
for action IDs, bounded batches and partial-failure recovery.

For a hosted session, change only the URL to the exact MCP endpoint supplied by
that session and use its existing access policy. Do not put access tokens in the
Codex configuration file or public workflow inputs.

## Model-visible screenshots

`android.observe`, `android.step` and `android.install_build_from_run` return a
fresh PNG with their receipt, including partial failures when capture succeeds.
Missing pixels are explicit errors. `android.capture_screenshot` returns the PNG as an MCP image content item and
keeps the serial, backend, and artifact path in a JSON text receipt. Image-bearing
results omit `structuredContent`: Codex prefers that field when present and does
not forward accompanying content blocks to the model. Results without an image
continue to use structured content.
`android.inspect_ui` and `android.wait_for_stable_ui` attach the same image item
when screenshot capture is enabled. Codex can use these namespaced tools through
its generic MCP configuration; no Codex-specific dynamic-tool adapter is
required. The artifact paths and hierarchy remain evidence for replay or
debugging, while the typed image is the model-visible visual observation.

### Coordinates after client image resizing

Coordinate input uses native Android pixels. The matching observation receipt's
`width` and `height` describe that native frame. At the qualified Sedna Codex
revision [`04a76a98ca1c6ebe79417fc5e904f7562c7a0a5b`](https://github.com/sednalabs/codex/blob/04a76a98ca1c6ebe79417fc5e904f7562c7a0a5b/codex-rs/core/src/image_preparation.rs),
high/default image processing applies a 2048-pixel maximum dimension and a
2500-patch budget. It can resize the PNG without changing the JSON receipt or
adding a coordinate transform.

When choosing a point from a resized image, use its actual pixel dimensions
`image_width` and `image_height` to map back to the receipt's native dimensions:

```text
x_native = round(x_image * native_width / image_width)
y_native = round(y_image * native_height / image_height)
```

Clamp each result to the native bounds, and submit it with the same fresh
`observation_id`. Recompute after rotation or another observation; do not assume
a fixed scale from a device profile. Prefer semantic selectors when available.
The scripted consumer checks qualify typed-image delivery and action receipts;
they do not establish live-model pointing accuracy. Other Codex revisions must
be checked against their own image-processing behavior.

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
