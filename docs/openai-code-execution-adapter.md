# OpenAI Code-Execution Adapter

This server already owns the Android-side control plane:

- emulator lifecycle
- ADB input
- screenshot capture
- UI hierarchy dumps
- logcat collection
- optional app-specific scripted journeys

The OpenAI computer-use/code-execution layer should sit on top of that control
plane rather than replace it.

This document assumes the underlying MCP remains on the simple proven `rmcp`
runtime. Toolkit adoption should stay selective and should not block the
interaction-quality work that makes the harness useful.

## Why this shape

OpenAI computer-use guidance describes three viable paths:

- built-in computer tool
- custom tool or harness
- code-execution harness

For this repo, the best fit is a code-execution harness layered over the
existing MCP server. That keeps Android lifecycle, retries, artifact collection,
and safety boundaries in one place while giving the model a persistent runtime
to mix visual and programmatic interaction.

## Recommended adapter contract

The code-execution runtime should expose a small helper surface to the model:

- `listDevices()`
- `launchAvd(...)`
- `waitForBoot(...)`
- `installApk(...)`
- `launchApp(...)`
- `captureScreenshot(...)`
- `dumpUiHierarchy(...)`
- `collectLogcat(...)`
- `tap(...)`
- `typeText(...)`
- `swipe(...)`
- `keyevent(...)`
- optional app-specific scenario helpers, when the target app provides them
- `displayScreenshot(path)`

For the runtime helper layer, prefer exposing both:

- raw device helpers
- semantic UI helpers that wrap the MCP's normalized selector tools

That means the runtime helper library should include:

- `inspectUi(...)`
- `waitForStableUi(...)`
- `findUiElement(selector, ...)`
- `waitForUiElement(selector, ...)`
- `tapElement(selector, ...)`
- `typeIntoElement(selector, text, ...)`
- `scrollUntilVisible(selector, ...)`

For modal and prompt flows, prefer `tapElement(selector, { waitUntilAbsent:
true })` or pass `waitForSelector` so the runtime only treats the action as
successful once the UI actually transitions. The helper now throws when that
verification fails unless `allowVerificationFailure: true` is set explicitly.
Use `waitUntilAbsent` when you need proof that a prompt or tapped control really
left the screen. `waitForSelector` proves post-tap presence, and when that
follow-up selector was already visible before dispatch the MCP also requires the
normalized hierarchy to change before it treats the tap as verified. Verified
taps also wait for the satisfied post-tap state to remain stable across
repeated hierarchy-plus-window observations, so callers can trust the result
more than a single delayed snapshot.

For `waitForUiElement(...)` and `scrollUntilVisible(...)`, treat `ok` as the
success bit. The runtime helper now throws when those tools time out or exhaust
their swipe budget so agents do not accidentally continue on a stale world
model. `typeIntoElement(...)` now waits for a stable post-tap UI state before
sending text and returns that stabilized observation in its artifact payload.

The important boundary is:

- the MCP server remains the source of truth for device lifecycle and artifact
  storage
- the code-execution runtime remains the source of truth for short-lived script
  logic, loops, and visual interaction flow

## Deferred loading and tool-search stratification

For multimodal Responses-style clients, do not assume the full Android tool
surface is always eagerly visible at session start.

The runtime should treat tool loading as stratified:

- eager bootstrap lane:
  - `android.health`
  - `android.list_avds`
  - `android.list_devices`
  - `android.launch_avd`
  - `android.launch_avd_and_wait`
  - `android.wait_for_boot`
  - `android.install_apk`
  - `android.launch_app`
- deferred observation lane:
  - screenshot, hierarchy, settled-state inspection, logcat
- deferred semantic-ui lane:
  - selector-driven find/wait/tap/type/scroll tools
- deferred raw-input lane:
  - coordinate and keyevent fallbacks
- deferred app-specific scenario lane:
  - scenario tools and narrow domain actions

The runtime example now encodes that policy directly rather than burying it in
prompt text. It accepts optional host hooks for:

- `ensureToolsAvailable(...)`
- `toolSearch(...)`

and invokes them only for deferred groups. That lets a client pay for the
heavier schemas only when it actually crosses into semantic UI, raw input, or
domain-specific work.

This is the current best fit for our Android harness:

- keep the MCP truthful and well-described
- keep the runtime aware of eager vs deferred lanes
- avoid assuming Codex or another client must eagerly surface every Android tool
  up front
- use tool-search as a supported loading path rather than treating it as an
  anomaly

## Persistent runtime expectations

OpenAI code-execution guidance favors a persistent runtime when it
improves efficiency. For this adapter, persistence is valuable because the
runtime can keep:

- a long-lived MCP client connection
- the current emulator serial
- the last screenshot path
- the last dumped UI hierarchy path
- small helper functions for repeated flows

The repo now includes a concrete helper sketch for that shape:

- `docs/examples/android_emulator_runtime_client.js`
- optional app-specific helper modules such as
  `docs/examples/solarlab_code_execution_helpers.js`
- `docs/examples/android_computer_bridge.js`

The generic runtime client owns persistent session state:

- current serial
- last screenshot path
- last UI dump path
- last scenario result
- loaded deferred tool groups

App-specific helpers stay thin on top of the generic runtime.

## Native Responses Item contract

The runtime should treat local artifact paths as a host-owned implementation
detail, not the model-facing contract.

For multimodal Responses loops, the useful boundary is:

- screenshots flow back as `input_image`
- UI dumps, manifests, and logs flow back as `input_file`
- the host decides whether those items are backed by:
  - uploaded `file_id` handles
  - externally served HTTPS URLs
  - or inline data URLs for small local proof loops

That is closer to OpenAI Responses guidance than handing the model a
raw filesystem path and expecting a follow-up turn to recover the bytes.

The repo now includes a helper sketch for this shape:

- `docs/examples/android_responses_items.js`
- `adapters/openai/`

That adapter:

- maps screenshot ownership into `input_image` items
- maps XML, manifest, and log artifacts into `input_file` items
- prefers host-brokered uploaded `file_id` references when available, including
  screenshots during longer hosted loops
- falls back to hosted URLs or inline data URLs when the host has not yet
  implemented upload brokering

This keeps the contract clean:

- the MCP still owns artifact creation and storage
- the runtime still owns short-lived orchestration state
- the Responses item adapter owns the model-facing multimodal packaging step

That still leaves an important product boundary:

- a standalone runner-local helper that calls OpenAI directly is useful for
  proof and fallback operation
- but it is not the same thing as Codex CLI reusing its own active session

For Codex-first use, prefer the Codex bridge direction:

- upstream Codex already supports `input_image` items and `function_call_output`
  image content
- upstream Codex `js_repl` also supports `codex.emitImage(...)`
- upstream app-server already supports `thread/inject_items`

So the honest Codex-native path is:

- package Android observations into Codex-compatible raw Responses items
- append them to a Codex thread when a thread/app-server transport handle is
  available
- avoid pretending a runner-local shell script can silently reuse the active
  Codex session without that handle

## Minimal durable run-state seam

The next step toward a real execution harness is not a giant rewrite. It is a
small run object with:

- a durable `runId`
- JSON-serializable run state
- checkpointable step history
- enough structure to resume after an interrupted session

The repo now includes a minimal example of that shape:

- `docs/examples/android_execution_run.js`

This example is intentionally narrow:

- it sits above the review-session helper layer
- it can attach a serial, capture a baseline, run a scenario review, and export
  a resumable checkpoint
- it does not yet claim to be a hardened sandbox boundary

That keeps the evolution incremental:

- model the durable run-state boundary
- keep the proven loopback MCP and runtime helpers intact
- leave a stronger sandbox-host split as a later hardening step instead of
  pretending it already exists

## Computer-Style Action Bridge

The next bridge layer should look more like a computer-use action adapter than a
plain helper bag.

For our Android harness, that means:

- keep the runtime and semantic MCP tools as the preferred lane
- add a small bridge that can accept batched computer-style actions
- keep explicit view and zoom state in the bridge
- remap action coordinates from the model-visible frame into device coordinates
- use an optional crop hook when a zoomed screenshot artifact is needed

The repo now includes a bridge sketch for this shape:

- `docs/examples/android_computer_bridge.js`

That bridge demonstrates:

- batched action execution
- explicit zoom and reset-zoom actions
- coordinate remapping through the current zoomed region
- scroll-to-swipe translation for Android fallback control
- capture of view metadata alongside screenshot artifacts

This keeps the architecture honest:

- the Android MCP remains authoritative for lifecycle, artifacts, and semantic tools
- the runtime remains authoritative for session-local orchestration
- the computer bridge remains a thin action translation layer, not a new control plane

## Freeform exploration loop

Once the first-class scripted journeys are proven, the next layer should stay
thin:

- resume from the last captured screenshot and UI dump when they already exist
- capture a fresh paired screenshot and UI dump only when the runtime is stale
- wrap one bounded freeform action in a small `observe -> act -> observe` loop

In the example runtime, that contract is expressed as:

- `getVisualContext({ includeUi = true })`
- `ensureVisualContext(label, { includeUi = true, display = true })`
- `runExplorationStep(label, action, { captureBefore = false, captureAfter = true, includeUi = true, display = true })`

That keeps the runtime lightweight:

- the MCP remains the source of truth for artifact paths
- the runtime remains the place where a code-execution session can cheaply
  resume, take one exploratory step, and inspect the resulting screenshot/UI
  state without rebuilding context each turn
- when a fresh capture is needed, the helper returns the refreshed screenshot
  and UI paths directly in the context object

## Screenshot and image flow

The adapter should standardize this pattern:

1. call one helper that captures the current state
2. save the screenshot path and, when `includeUi` is true, the UI dump path into persistent runtime state
3. re-emit the screenshot to the model at high fidelity
4. keep the paired `uiautomator` XML path available for precise follow-up

That gives us a useful hybrid:

- visual understanding from screenshots
- precise fallback targeting from the XML tree
- a bounded settled-state wait when the UI is in motion

In the example runtime, that contract is expressed as:

- `captureState(label, { includeUi = true, display = true })`
- `displayScreenshot(path)`
- `displayLastScreenshot()`

For visual loops, screenshot-return helpers default to `detail: "original"` so
the adapter preserves full-resolution screenshots unless the caller deliberately
lowers fidelity.

## Optional Scenario Journeys

The repository includes these app-specific journeys as optional examples:

- `stage_first_focus_earth`
- `stage_first_immersive_roundtrip`

Those are narrow scenario tools layered over the generic Android control surface.
They are not required for generic emulator automation.

## Initial integration sequence

1. Start the local MCP server and connect it through the OpenAI config.
2. Expose a single code-execution tool with persistent helper state.
3. Add helper functions that delegate to the MCP tools instead of shelling out
   directly.
4. Prove one complete run:
   - boot or attach emulator
   - install APK
   - run a narrow scenario or semantic UI action
   - emit the final screenshot back to the model
5. After that, add a screenshot-first freeform exploration loop for harder UX
   review.

## Current example layering

- `docs/examples/android_emulator_runtime_client.js`
  - generic persistent runtime over MCP
  - keeps session-local state and delegates lifecycle/artifacts to MCP tools
  - now includes raw device helpers plus semantic UI helper wrappers
  - now also encodes eager vs deferred tool groups and optional loading hooks
- `docs/examples/android_responses_items.js`
  - maps screenshot and file artifacts into native Responses content items
  - keeps raw artifact paths behind the runtime boundary
- `adapters/openai/`
  - supported package entrypoint for the runtime, Responses item adapter,
    Streamable HTTP MCP client, host-side OpenAI file broker, thin Responses
    loop driver, and native `function_call_output` / `computer_call_output`
    helpers
- `docs/examples/android_execution_run.js`
  - minimal durable run-state seam above the runtime and review-session layers
  - exports resumable checkpoint state instead of only ephemeral helper results
- `docs/examples/solarlab_code_execution_helpers.js`
  - optional app-specific overlay for the two current scenario journeys
- `docs/examples/android_computer_bridge.js`
  - computer-style batched action bridge with explicit zoom/view state
  - keeps coordinate remapping and raw fallback logic above the runtime
- `docs/examples/solarlab_review_session.js`
  - scenario-first review-session example for one app-specific domain
  - keeps the operating order explicit: scenario first, freeform second

## Keep out of scope

Do not turn this into a general desktop automation platform yet.

The next thin slice is:

- one OpenAI-connected runtime
- one persistent helper layer
- one Android emulator
- one target app
- one proven screenshot-return path

## Cost discipline

To keep the code-execution loop efficient, prefer this order:

1. reuse the runtime's last screenshot and UI dump when they are still truthful
2. run a narrow MCP-backed scenario or semantic helper first
3. only escalate to `runExplorationStep(...)` when the scripted path did not
   answer the UX question
4. collect logcat or larger artifacts when something fails or when you want a
   final proof bundle, not after every tiny interaction

That keeps the expensive visual loop focused on the places where it adds real
value instead of spending tokens rediscovering state the MCP already knows.
