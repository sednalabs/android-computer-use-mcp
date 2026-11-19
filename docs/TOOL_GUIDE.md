# Tool Guide

`android-computer-use-mcp` exposes Android emulator control as small MCP tools with
structured results and artifact paths. The tools are grouped by how an agent
should normally use them.

## Recommended workflow

Discover and resolve an exact ready device, then use `android.observe` and
`android.step` for ordinary agent interaction. Preserve each action ID and use
`android.get_action_status` after a lost response or provider restart. Use
`android.install_build_from_run` for a hosted build when its operator
configuration is enabled. These tools run through generic MCP, with no native
Codex Android integration.

See [Agent workflows](agent-workflows.md) for complete examples, receipt fields,
coordinate freshness and non-replay semantics. Low-level tools remain available
for focused inspection and specialised input. They share device leases but do
not inherit the composite workflow's durable action-ID guarantee.

## Core Discovery

- `android.health` reports server configuration, backend posture, and artifact
  directory state.
- `android.list_avds` lists Android Virtual Devices known to the configured SDK.
- `android.list_devices` lists attached or running Android devices.
- `android.resolve_target` validates a ready serial and exact provider/session identity.

## Lifecycle

- `android.launch_avd` starts an AVD.
- `android.launch_avd_and_wait` starts an AVD and waits for readiness.
- `android.wait_for_boot` waits for an existing device to finish booting.
- `android.install_apk` installs an APK on a target device.
- `android.launch_app` launches a package/activity and verifies requested
  postconditions when supplied.

- `android.list_apps`, `android.terminate_app`, `android.uninstall_app` and
  `android.open_url` provide explicit app lifecycle operations.
- `android.get_orientation` and `android.set_orientation` use observed default-display rotation.

## Observation

- `android.observe` returns model-visible pixels, compact UI and an observation ID.
- `android.capture_screenshot` writes a PNG artifact and returns a typed image.
- `android.read_artifact` reads a bounded, root-confined artifact.
- `android.dump_ui_hierarchy` writes a UIAutomator XML artifact.
- `android.collect_logcat` writes a logcat artifact.
- `android.inspect_ui` captures a paired screenshot/UI dump with normalized UI
  state.
- `android.wait_for_stable_ui` waits for hierarchy and top-window metadata to
  settle before returning the observation bundle.

Observation tools should be preferred whenever the next decision depends on
what the user or model can actually see.

## Semantic UI

- `android.find_ui_element` resolves a selector and reports whether it matched
  uniquely.
- `android.wait_for_ui_element` waits until a selector resolves.
- `android.tap_element` taps a resolved element and can verify post-tap state.
- `android.type_into_element` focuses a resolved input and verifies visible text
  entry.
- `android.scroll_until_visible` scrolls until a target selector appears or the
  swipe budget is exhausted. Its direction describes vertical finger motion:
  `up` (the default) swipes from bottom to top to reveal later rows in a list;
  `down` swipes from top to bottom. Horizontal directions are unsupported.

Selector-bearing tools accept a structured selector object. Some tools also
accept a string shorthand such as `"Search"` for visible text. Ambiguity is a
failure by default; use a more specific selector or `match_index` when multiple
nodes are valid targets.

## Raw Input

- `android.input.tap`
- `android.input.text`
- `android.input.swipe`
- `android.input.keyevent`
- `android.input.double_tap` and `android.input.long_press`
- `android.input.multi_touch` (atomic emulator gRPC only)
- `android.input.keycombination`

Raw input is useful for fallback control, but it should not be the first choice
when a stable semantic selector exists. Raw actions can include postcondition
checks so callers do not treat dispatch alone as proof of success.

## Hosted Interactive Sessions

- `interactive_session.get_status`
- `interactive_session.get_current_build`
- `interactive_session.install_build_from_run`
- `interactive_session.relaunch_current_build`

These tools support runner-backed sessions where a live emulator can be reused
across APK installs and app relaunches. Configure them only when the host owns
the runner, artifact root, repository, and credentials.

## Optional Scenario Tools

- `solarlab.scenario.stage_first_focus_earth`
- `solarlab.scenario.stage_first_immersive_roundtrip`
- `solarlab.semantic_action`

These are app-specific examples layered over the generic Android control
surface. They are part of the current public schema snapshot, but generic
Android workflows should not eagerly load or depend on them unless that target
app is the task.

## Tool-Loading Guidance

For clients that support deferred tool loading, keep the bootstrap tools eager
and load heavier groups only when needed:

- eager: health, target resolution, observe, step and action status
- deferred observation: screenshots, UI XML, stable UI, logcat
- deferred semantic UI: selector-driven find, wait, tap, type, scroll
- deferred raw input: coordinate/key fallbacks
- deferred scenario tools: app-specific scenario flows

That keeps the initial tool surface small while preserving the full Android
automation contract for sessions that need it.

## Artifacts

Artifact paths are server-owned implementation details. Adapters should convert
screenshots, XML, logcat, and manifests into model-native image or file items
instead of asking a model to reason over raw local filesystem paths.
