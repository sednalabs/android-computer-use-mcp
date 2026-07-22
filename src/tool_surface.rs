//! Public tool-surface metadata for android-computer-use-mcp.
//!
//! ## Rationale
//! Centralizes the public tool inventory so list/call gating, discovery resources,
//! and contract tests all share one source of truth.
//!
//! ## Security Boundaries
//! * Read-only classification is explicit and reviewable.
//! * Group labels are stable policy inputs for future allowlists and profiles.

use mcp_toolkit_core::tool_inventory::{ToolCapability, ToolInventory, ToolInventoryError};
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub(crate) struct ToolSurfaceEntry {
    pub(crate) name: &'static str,
    pub(crate) group: &'static str,
    pub(crate) read_only: bool,
    pub(crate) description: &'static str,
    pub(crate) use_when: &'static str,
}

const TOOL_SURFACE: [ToolSurfaceEntry; 41] = [
    ToolSurfaceEntry {
        name: "interactive_session.get_status",
        group: "interactive-session",
        read_only: true,
        description: "Report hosted interactive-session configuration, live session root, and active build metadata.",
        use_when: "Use this first when you need to understand the current hosted runner-backed session before switching builds.",
    },
    ToolSurfaceEntry {
        name: "interactive_session.get_current_build",
        group: "interactive-session",
        read_only: true,
        description: "Read the current active-build metadata for the hosted interactive session.",
        use_when: "Use this when you want the exact build currently installed without changing live session state.",
    },
    ToolSurfaceEntry {
        name: "interactive_session.install_build_from_run",
        group: "interactive-session",
        read_only: false,
        description: "Download a reusable interactive build artifact from a GitHub Actions run, install it into the live session, and optionally relaunch the app.",
        use_when: "Use this to swap a newly built APK into an already-live hosted session instead of restarting the runner.",
    },
    ToolSurfaceEntry {
        name: "interactive_session.relaunch_current_build",
        group: "interactive-session",
        read_only: false,
        description: "Relaunch the current active build inside the live hosted interactive session.",
        use_when: "Use this for a cheap retry loop when the active build is already installed and only needs relaunch.",
    },
    ToolSurfaceEntry {
        name: "android.health",
        group: "status",
        read_only: true,
        description: "Report SDK paths, known AVDs, and attached Android devices.",
        use_when: "Use this first to understand the local Android harness posture before taking action.",
    },
    ToolSurfaceEntry {
        name: "android.list_avds",
        group: "emulator-lifecycle",
        read_only: true,
        description: "List known local Android virtual devices.",
        use_when: "Use this when you need to choose or verify an AVD name before launch.",
    },
    ToolSurfaceEntry {
        name: "android.list_devices",
        group: "status",
        read_only: true,
        description: "List adb-visible Android devices.",
        use_when: "Use this when you need the current device serials or want to confirm device visibility.",
    },
    ToolSurfaceEntry {
        name: "android.resolve_target",
        group: "status",
        read_only: true,
        description: "Resolve one exact Android provider, session, and device target.",
        use_when: "Use this before a target-bound Android operation to reject a stale or mismatched provider session.",
    },
    ToolSurfaceEntry {
        name: "android.launch_avd",
        group: "emulator-lifecycle",
        read_only: false,
        description: "Launch a named AVD in the background.",
        use_when: "Use this to start an emulator when no suitable device is already running.",
    },
    ToolSurfaceEntry {
        name: "android.launch_avd_and_wait",
        group: "emulator-lifecycle",
        read_only: false,
        description: "Launch a named AVD, wait for its emulator serial to appear, and verify boot readiness.",
        use_when: "Use this when you want one bounded boot flow instead of separate launch and boot checks.",
    },
    ToolSurfaceEntry {
        name: "android.wait_for_boot",
        group: "emulator-lifecycle",
        read_only: true,
        description: "Wait until sys.boot_completed=1 and package manager is responsive.",
        use_when: "Use this after launch when the device exists but may not be ready for app work yet.",
    },
    ToolSurfaceEntry {
        name: "android.install_apk",
        group: "app-control",
        read_only: false,
        description: "Install an APK onto the target device.",
        use_when: "Use this when the app under test is not already installed or needs updating.",
    },
    ToolSurfaceEntry {
        name: "android.launch_app",
        group: "app-control",
        read_only: false,
        description: "Launch an installed Android app by package name and optional activity.",
        use_when: "Use this to bring an app to the foreground before semantic UI interaction.",
    },
    ToolSurfaceEntry {
        name: "android.list_apps",
        group: "app-control",
        read_only: true,
        description: "List installed Android apps, optionally limited to launcher-visible apps.",
        use_when: "Use this when you need package names before launching, terminating, or uninstalling apps.",
    },
    ToolSurfaceEntry {
        name: "android.terminate_app",
        group: "app-control",
        read_only: false,
        description: "Force-stop an installed Android app by package name.",
        use_when: "Use this to reset app state without uninstalling it.",
    },
    ToolSurfaceEntry {
        name: "android.uninstall_app",
        group: "app-control",
        read_only: false,
        description: "Uninstall an Android app by package name.",
        use_when: "Use this only when the app should be removed from the device.",
    },
    ToolSurfaceEntry {
        name: "android.open_url",
        group: "app-control",
        read_only: false,
        description: "Open an http:// or https:// URL on the Android device.",
        use_when: "Use this when the browser or URL intent path is the task under test.",
    },
    ToolSurfaceEntry {
        name: "android.get_orientation",
        group: "status",
        read_only: true,
        description: "Read the device user_rotation setting and normalized orientation class.",
        use_when: "Use this to record orientation before choosing gestures or validation expectations.",
    },
    ToolSurfaceEntry {
        name: "android.set_orientation",
        group: "app-control",
        read_only: false,
        description: "Set the device user_rotation after disabling accelerometer rotation.",
        use_when: "Use this when validation needs a known portrait or landscape posture.",
    },
    ToolSurfaceEntry {
        name: "android.capture_screenshot",
        group: "observation",
        read_only: false,
        description: "Capture a PNG screenshot from the device and save it locally.",
        use_when: "Use this when a visual artifact is more important than the normalized UI tree.",
    },
    ToolSurfaceEntry {
        name: "android.dump_ui_hierarchy",
        group: "observation",
        read_only: false,
        description: "Dump the current UI hierarchy XML and save it locally.",
        use_when: "Use this when you need raw hierarchy XML for debugging or later parsing.",
    },
    ToolSurfaceEntry {
        name: "android.read_artifact",
        group: "observation",
        read_only: true,
        description: "Read a previously generated artifact file from the configured artifact directory and return its contents for remote consumers.",
        use_when: "Use this when a remote consumer needs the bytes or text for a screenshot, UI dump, or other artifact path that was returned earlier.",
    },
    ToolSurfaceEntry {
        name: "android.inspect_ui",
        group: "observation",
        read_only: false,
        description: "Capture the current UI hierarchy, optionally pair it with a screenshot, and return a normalized UI tree.",
        use_when: "Use this for a rich one-shot observation of the current Android state.",
    },
    ToolSurfaceEntry {
        name: "android.wait_for_stable_ui",
        group: "observation",
        read_only: false,
        description: "Wait until the UI hierarchy and top window state stop changing, then return a paired observation bundle.",
        use_when: "Use this before acting when the UI may still be animating or transitioning.",
    },
    ToolSurfaceEntry {
        name: "android.find_ui_element",
        group: "semantic-ui",
        read_only: false,
        description: "Find the first normalized UI element matching a semantic selector in the current hierarchy.",
        use_when: "Use this to inspect whether a target control is present and how it resolved before acting.",
    },
    ToolSurfaceEntry {
        name: "android.wait_for_ui_element",
        group: "semantic-ui",
        read_only: false,
        description: "Wait until a semantic UI selector is present or absent in the current hierarchy.",
        use_when: "Use this when a UI transition is expected and success depends on a selector appearing or disappearing.",
    },
    ToolSurfaceEntry {
        name: "android.tap_element",
        group: "semantic-ui",
        read_only: false,
        description: "Find a semantic UI element and tap its center point.",
        use_when: "Use this as the default interaction path when you can describe the target semantically.",
    },
    ToolSurfaceEntry {
        name: "android.type_into_element",
        group: "semantic-ui",
        read_only: false,
        description: "Find a semantic UI element, tap it, and send text input.",
        use_when: "Use this for text entry into a known field when semantic targeting is available.",
    },
    ToolSurfaceEntry {
        name: "android.scroll_until_visible",
        group: "semantic-ui",
        read_only: false,
        description: "Swipe the viewport until a semantic UI selector becomes visible or the swipe budget is exhausted.",
        use_when: "Use this when the target probably exists off-screen and semantic scrolling is preferable to raw swipes.",
    },
    ToolSurfaceEntry {
        name: "android.collect_logcat",
        group: "observation",
        read_only: false,
        description: "Capture recent logcat output and save it locally.",
        use_when: "Use this after a failure or suspicious behavior when app/runtime logs matter.",
    },
    ToolSurfaceEntry {
        name: "android.input.tap",
        group: "raw-input",
        read_only: false,
        description: "Send a tap input event to the device.",
        use_when: "Use this only when semantic selection is insufficient and you already know the screen coordinates.",
    },
    ToolSurfaceEntry {
        name: "android.input.double_tap",
        group: "raw-input",
        read_only: false,
        description: "Send a double-tap input event to the device as one bounded gesture.",
        use_when: "Use this only when a genuine Android double-tap gesture is needed and semantic interaction is insufficient.",
    },
    ToolSurfaceEntry {
        name: "android.input.long_press",
        group: "raw-input",
        read_only: false,
        description: "Send a long-press input event to the device as one bounded gesture.",
        use_when: "Use this only when a genuine Android long-press gesture is needed and semantic interaction is insufficient.",
    },
    ToolSurfaceEntry {
        name: "android.input.text",
        group: "raw-input",
        read_only: false,
        description: "Send text input to the device.",
        use_when: "Use this when raw keyboard-style entry is required after focus is already established.",
    },
    ToolSurfaceEntry {
        name: "android.input.swipe",
        group: "raw-input",
        read_only: false,
        description: "Send a swipe gesture to the device.",
        use_when: "Use this only when semantic scrolling is insufficient or you need an exact gesture path.",
    },
    ToolSurfaceEntry {
        name: "android.input.multi_touch",
        group: "raw-input",
        read_only: false,
        description: "Send two to five pointer paths as one atomic emulator gRPC gesture.",
        use_when: "Use this for pinch, two-finger pan, or another gesture that must keep every pointer in the same device frame.",
    },
    ToolSurfaceEntry {
        name: "android.input.keyevent",
        group: "raw-input",
        read_only: false,
        description: "Send a keyevent to the device.",
        use_when: "Use this for bounded low-level actions like Back or Enter when semantic tools are not the right fit.",
    },
    ToolSurfaceEntry {
        name: "android.input.keycombination",
        group: "raw-input",
        read_only: false,
        description: "Send a chorded key combination to the device.",
        use_when: "Use this for computer-use key chords such as Ctrl+C that must be dispatched atomically.",
    },
    ToolSurfaceEntry {
        name: "solarlab.scenario.stage_first_focus_earth",
        group: "solarlab-scenarios",
        read_only: false,
        description: "Launch Solar Lab, open search, focus Earth, and capture step-by-step artifacts.",
        use_when: "Use this to run a durable Stage First Solar Lab scenario rather than reassembling the flow by hand.",
    },
    ToolSurfaceEntry {
        name: "solarlab.scenario.stage_first_immersive_roundtrip",
        group: "solarlab-scenarios",
        read_only: false,
        description: "Launch Solar Lab, open immersive view, return to sandbox, and capture artifacts.",
        use_when: "Use this to validate the immersive roundtrip flow with consistent artifact capture.",
    },
    ToolSurfaceEntry {
        name: "solarlab.semantic_action",
        group: "solarlab-scenarios",
        read_only: false,
        description: "Send a narrow semantic action into the Solar Lab app and optionally capture post-action screenshot and UI artifacts.",
        use_when: "Use this for narrow Solar Lab domain actions when a full scenario would be too broad.",
    },
];

pub(crate) fn tool_surface_entries() -> &'static [ToolSurfaceEntry] {
    &TOOL_SURFACE
}

pub(crate) fn build_tool_inventory() -> Result<ToolInventory, ToolInventoryError> {
    ToolInventory::from_capabilities(tool_surface_entries().iter().map(|entry| {
        ToolCapability::new(entry.name)
            .with_group(entry.group)
            .with_read_only(entry.read_only)
    }))
}
