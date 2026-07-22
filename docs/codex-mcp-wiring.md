# Codex MCP Wiring

This server is ready to be wired into Codex as a local loopback Streamable HTTP MCP.

## Recommended Codex config snippet

Add a block like this to `~/.codex/config.toml`:

```toml
[mcp_servers.android_computer_use_mcp]
url = "http://127.0.0.1:9526/mcp"
tool_timeout_sec = 300.0
enabled = false
```

Keep `enabled = false` until the operator is ready to turn the server on in
their active Codex profile.

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

## Exact hosted target identity

When more than one hosted Android candidate can exist at once, configure a
stable execution identity before publishing the provider:

```bash
export ANDROID_COMPUTER_USE_MCP_ENVIRONMENT_ID="<environment-id>"
export ANDROID_COMPUTER_USE_MCP_PROVIDER_INSTANCE_ID="<provider-instance-id>"
export ANDROID_COMPUTER_USE_MCP_SESSION_ID="<session-id>"
```

Callers can use `android.resolve_target` to prove that tuple and the requested
device serial belong to the live provider before a target-bound action. The
Codex provider manifest carries the same tuple in schema v5. Persisted v4
manifests must be regenerated before validation or use; this metadata migration
does not remove the legacy install-input translation retained for compatible
callers.

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
