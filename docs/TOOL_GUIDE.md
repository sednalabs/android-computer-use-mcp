# Tool Guide

`android-computer-use-mcp` exposes Android emulator control as small MCP tools with
structured results and artifact paths. The tools are grouped by how an agent
should normally use them.

## Recommended Order

1. Discover the harness state with `android.health`, `android.list_devices`, and
   `android.list_avds`.
2. Launch or attach to a device with `android.launch_avd_and_wait` or
   `android.wait_for_boot`.
3. Install and launch the target app with `android.install_apk` and
   `android.launch_app`.
4. Observe with `android.wait_for_stable_ui` or `android.inspect_ui`.
5. Act with semantic tools such as `android.tap_element` and
   `android.type_into_element`.
6. Fall back to raw input only when semantic targeting cannot express the action.
7. Capture proof with screenshots, UI XML, logcat, or scenario manifests.

## Core Discovery

- `android.health` reports server configuration, backend posture, and artifact
  directory state.
- `android.list_avds` lists Android Virtual Devices known to the configured SDK.
- `android.list_devices` lists attached or running Android devices.

## Lifecycle

- `android.launch_avd` starts an AVD.
- `android.launch_avd_and_wait` starts an AVD and waits for readiness.
- `android.wait_for_boot` waits for an existing device to finish booting.
- `android.install_apk` installs an APK on a target device.
- `android.launch_app` launches a package/activity and verifies requested
  postconditions when supplied.

## Observation

- `android.capture_screenshot` writes a PNG artifact.
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
  swipe budget is exhausted.

Selector-bearing tools accept a structured selector object. Some tools also
accept a string shorthand such as `"Search"` for visible text. Ambiguity is a
failure by default; use a more specific selector or `match_index` when multiple
nodes are valid targets.

## Raw Input

- `android.input.tap`
- `android.input.text`
- `android.input.swipe`
- `android.input.keyevent`

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

- eager: health, device discovery, AVD launch, install, app launch
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
