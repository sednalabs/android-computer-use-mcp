# Codex Bridge Helpers

This package is the Codex-facing companion to `android-computer-use-mcp`.
Despite the historical MCP name, this is the reusable Android computer-use
harness boundary: Solar Lab is a proving app, not the owner of the generic
tool contract.

The native Codex harness path is the command-backed dynamic-tool host:

- `android_observe`
- `android_step`
- `android_install_build_from_run`

That host is meant to sit behind a real Codex dynamic-tool surface in the TUI.
The shell helper is the provider backend, not the primary user-facing control
plane.

Current contract direction:

- `android_observe` stays screenshot-first and returns a model-visible image plus
  concise text summary.
- `android_step` now prefers batched `actions[]` in a computer-style shape:
  `click`, `double_click`, `long_press`, `scroll`, `type`, `wait`, `keypress`,
  `drag`, `move`, with Android-specific `launch_app`, `open_url`,
  `set_orientation`, `zoom`, and `reset_zoom`.
- Text-only `android_observe` / post-action `android_step` output is degraded,
  not successful native computer use. A successful visual response must include
  an `inputImage` item; `visible_ui` text is useful secondary structure but not
  a substitute for pixels.
- The provider CodeQL contract checks `android_observe`, post-action
  `android_step`, and post-install `android_install_build_from_run` for the
  native-image warning path so install-and-observe loops cannot drift back to
  text-only success.
- Direct MCP screenshot-producing tools now return both structured JSON and MCP
  image content. `structuredContent` carries paths, UI digests, and state;
  `content[]` carries the screenshot bytes so native Codex computer-use bridges
  can hand pixels to the model without chasing artifact paths.
- `android_install_build_from_run` maps to the provider-side
  `interactive_session.install_build_from_run` helper, so a remote-built APK can
  be swapped into the active hosted session without pretending install is a UI
  gesture.
- `double_click` prefers `runtime.doubleTap(...)`, which maps to
  `android.input.double_tap` when the standard runtime client is used. This
  avoids the latency of two separate host-side tap tool calls.
- `long_press` prefers `runtime.longPress(...)`, which maps to
  `android.input.long_press` and keeps the gesture in one MCP call.
- multi-key `keypress` actions that include a modifier prefer
  `runtime.keyCombination(...)`, which maps to `android.input.keycombination`
  instead of dispatching independent keyevents.
- legacy single-action fields remain supported for compatibility while the
  computer-style contract becomes the preferred path.
- semantic taps accept `target` as an alias for `selector`, so
  `{"action":"tap","target":"Search"}` resolves through the semantic tap path
  rather than requiring raw `x`/`y` coordinates.
- selector objects normalize Codex-facing `content_description`,
  `contentDescription`, and `resourceId` aliases before MCP dispatch, while a
  UI-tree `bounds` target is tapped at its center instead of being sent as an
  unsupported semantic selector.
- selector-backed text entry uses the provider's verified
  `android.type_into_element` path, and `focus_body` semantic actions map their
  target to the provider's `body_query` field. Each Solar Lab semantic action
  carries a unique request ID; new app builds expose that ID, the original
  query, and the resolved body ID in an accessibility acknowledgement so the
  provider cannot accept stale UI state when aliases such as `comet` resolve to
  a canonical object. Literal-name matching remains as a compatibility path for
  older app builds.
- absent-selector waits run after the tap as their own verified transition, so
  a caller does not accidentally request that the same selector be both present
  and absent.
- when `view` metadata is provided, the helper keeps zoom/view state and remaps
  action coordinates through the current visible frame before returning a fresh
  screenshot plus updated view metadata summary.
- tool responses include provider-owned `metadata.android.outcome` so callers
  can distinguish success, degraded observation, unsatisfied postconditions, and
  retry posture without scraping the text summary.

This package is intentionally different from the standalone OpenAI Responses helper:

- it does not make direct OpenAI API calls
- it does not require `OPENAI_API_KEY`
- it keeps `android-computer-use-mcp` as the Android control plane
- it can either:
  - resolve native Codex dynamic tool calls through `codex-android-tools.mjs`
  - or produce Codex-ready raw Responses items and `thread/inject_items`
    payloads for explicit bridge/debug workflows

Current entrypoints:

- `createCodexThreadItemsAdapter`
- `createCodexAndroidDynamicToolHost`
- `createMessageItem`
- `createThreadInjectItemsParams`
- `contextFromObservation`

The package also ships a CLI:

- `bin/codex-android-tools.mjs`
- `bin/codex-android-observe.mjs`

`codex-android-tools.mjs` is the native provider backend CLI. It talks to a
live `android-computer-use-mcp` Streamable HTTP endpoint and serves:

- `specs`
- `call`
- `manifest`

for the Codex TUI dynamic-tool host.

It is designed to be called directly by Codex:

- executable via its own shebang
- defaults to `~/.codex/android-dynamic-tools.json`, with
  `~/.codex/solarlab-android-dynamic-tools.json` retained as a legacy fallback
- picks up Cloudflare Access service-token headers from:
  - `CODEX_ANDROID_MCP_CF_ACCESS_CLIENT_ID`
  - `CODEX_ANDROID_MCP_CF_ACCESS_CLIENT_SECRET`
- can derive the MCP URL from either config, `CODEX_ANDROID_MCP_URL`, or
  `CODEX_ANDROID_MCP_HOSTNAME`
- still accepts the older `SOLARLAB_ANDROID_*` environment aliases for hosted
  Solar sessions that have not migrated yet
- keeps `specs` available without opening a live MCP session, and reports
  transient provider loss during `call` as a structured
  `provider_unavailable` Android outcome rather than a raw transport exception
- emits a generic Android provider manifest for hosted sessions via
  `codex-android-tools.mjs manifest`

The provider manifest is intentionally generic Android capability metadata. It
identifies:

- provider family and adapter as Android
- native Codex tool names: `android_observe`, `android_step`, and
  `android_install_build_from_run`
- MCP transport URL and serial, when available
- exact environment, provider-instance, and session identifiers for rejecting
  stale hosted targets
- session, artifact, and active-build manifest paths
- default package/activity hints for app-focused sessions
- optional device metadata and capability groups for app control, posture, and
  raw-input support
- timeout and lease policy, including read-only observation, exclusive mutating
  step execution, and exclusive build-install execution
- outcome taxonomy for model-facing action status and retryability in manifest
  schema v2 and later

Manifest schema v5 is intentionally incompatible with persisted schema v4
manifests because v5 adds the exact execution identity required before a
mutating call. Regenerate a v4 manifest before validation or use; this metadata
migration is separate from the legacy tool-input translation supported by
`android_install_build_from_run`.

Solar Lab sessions can publish this manifest as proof that a hosted Android
runtime is available, but the manifest does not make Solar Lab the generic
provider owner.

Remote artifact reads are also intentionally model-facing rather than
host-facing. The helper consumes `android.read_artifact` payloads that include
the requested artifact path, encoding, MIME type, byte count, and payload, but
not the server's resolved absolute filesystem path.

`codex-android-observe.mjs` is the older explicit bridge/debug CLI. It captures
a fresh Android observation and emits either:

- a raw Codex-compatible `message` item
- or a ready-to-send `thread/inject_items` payload

Bridge/debug limitation:

- upstream Codex already supports `input_image` in raw thread items and
  `codex.emitImage(...)` inside `js_repl`
- but ordinary Codex CLI sessions do not currently expose the active thread
  transport handle automatically to arbitrary runner-local scripts

So this package supports both:

- native Codex dynamic-tool execution with no API key
- explicit bridge/debug output for `thread/inject_items` when a thread id and
  app-server transport are available
- no fake promise that a standalone shell script can silently mutate the active
  Codex conversation without that handle
