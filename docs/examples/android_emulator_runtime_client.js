// Minimal persistent runtime client for OpenAI code-execution sessions.
//
// The MCP remains the source of truth for device lifecycle and artifact paths.
// This helper only keeps session-local state so the model can mix tool calls
// and visual steps without rebuilding context on every turn.

export const ANDROID_TOOL_LOADING_PLAN = Object.freeze({
  bootstrap: Object.freeze({
    loading: "eager",
    useWhen:
      "Always-available bootstrap tools for discovery, device selection, launch, install, and app entry.",
    query: null,
    tools: Object.freeze([
      "android.health",
      "android.list_avds",
      "android.list_devices",
      "android.launch_avd",
      "android.launch_avd_and_wait",
      "android.wait_for_boot",
      "android.install_apk",
      "android.launch_app",
    ]),
  }),
  observation: Object.freeze({
    loading: "deferred",
    useWhen:
      "Observation-heavy tools for screenshots, hierarchy capture, settled-state inspection, and logs.",
    query: "android observation screenshot hierarchy inspect stable ui logcat",
    tools: Object.freeze([
      "android.capture_screenshot",
      "android.dump_ui_hierarchy",
      "android.inspect_ui",
      "android.wait_for_stable_ui",
      "android.collect_logcat",
    ]),
  }),
  semanticUi: Object.freeze({
    loading: "deferred",
    useWhen:
      "Preferred high-level Android interaction tools driven by semantic selectors.",
    query: "android semantic ui selectors tap type wait scroll visible",
    tools: Object.freeze([
      "android.find_ui_element",
      "android.wait_for_ui_element",
      "android.tap_element",
      "android.type_into_element",
      "android.scroll_until_visible",
    ]),
  }),
  rawInput: Object.freeze({
    loading: "deferred",
    useWhen:
      "Low-level fallback input tools for coordinates, swipes, keyboard events, and break-glass control.",
    query: "android raw input tap text swipe keyevent coordinates",
    tools: Object.freeze([
      "android.input.tap",
      "android.input.text",
      "android.input.swipe",
      "android.input.keyevent",
    ]),
  }),
  solarlab: Object.freeze({
    loading: "deferred",
    useWhen:
      "Solar Lab-specific scenarios and semantic actions that should not be eagerly loaded into generic Android sessions.",
    query: "solarlab stage first scenario semantic action immersive focus earth",
    tools: Object.freeze([
      "solarlab.scenario.stage_first_focus_earth",
      "solarlab.scenario.stage_first_immersive_roundtrip",
      "solarlab.semantic_action",
    ]),
  }),
});

const TOOL_GROUP_BY_NAME = Object.freeze(
  Object.fromEntries(
    Object.entries(ANDROID_TOOL_LOADING_PLAN).flatMap(([groupName, group]) =>
      group.tools.map((toolName) => [toolName, groupName]),
    ),
  ),
);

function cloneToolLoadingPlan() {
  return Object.fromEntries(
    Object.entries(ANDROID_TOOL_LOADING_PLAN).map(([groupName, group]) => [
      groupName,
      {
        ...group,
        tools: [...group.tools],
      },
    ]),
  );
}

export function createAndroidEmulatorRuntime({
  callMcp,
  displayScreenshot,
  defaultSerial = null,
  ensureToolsAvailable = null,
  toolSearch = null,
} = {}) {
  if (typeof callMcp !== "function") {
    throw new Error("createAndroidEmulatorRuntime requires a callMcp function");
  }

  const state = {
    currentSerial: defaultSerial,
    lastScreenshotPath: null,
    lastUiDumpPath: null,
    lastScenarioResult: null,
    loadedToolGroups: new Set(["bootstrap"]),
  };

  async function emitScreenshot(path) {
    if (!path) {
      throw new Error("no screenshot path was provided");
    }
    if (typeof displayScreenshot !== "function") {
      throw new Error("displayScreenshot helper was not provided");
    }
    await displayScreenshot(path);
    state.lastScreenshotPath = path;
    return path;
  }

  function throwIfNotOk(result, message) {
    if (result?.ok) {
      return result;
    }
    const detail = JSON.stringify(result);
    const error = new Error(`${message}: ${detail}`);
    error.result = result;
    throw error;
  }

  async function ensureToolGroupLoaded(groupName, reason) {
    const group = ANDROID_TOOL_LOADING_PLAN[groupName];
    if (!group) {
      throw new Error(`unknown Android tool group: ${groupName}`);
    }
    if (state.loadedToolGroups.has(groupName) || group.loading === "eager") {
      state.loadedToolGroups.add(groupName);
      return {
        group: groupName,
        loading: group.loading,
        tools: [...group.tools],
        query: group.query,
        resolvedVia: "already-available",
      };
    }

    const request = {
      group: groupName,
      loading: group.loading,
      tools: [...group.tools],
      query: group.query,
      reason,
      useWhen: group.useWhen,
    };

    if (typeof ensureToolsAvailable === "function") {
      await ensureToolsAvailable(request);
      state.loadedToolGroups.add(groupName);
      return { ...request, resolvedVia: "ensureToolsAvailable" };
    }

    if (typeof toolSearch === "function") {
      await toolSearch({
        query: group.query,
        group: groupName,
        toolNames: [...group.tools],
        reason,
      });
      state.loadedToolGroups.add(groupName);
      return { ...request, resolvedVia: "toolSearch" };
    }

    return { ...request, resolvedVia: "no-loader-provided" };
  }

  async function callAndroidTool(toolName, args, { reason } = {}) {
    const groupName = TOOL_GROUP_BY_NAME[toolName];
    if (groupName) {
      await ensureToolGroupLoaded(groupName, reason ?? `prepare ${toolName}`);
    }
    return callMcp(toolName, args);
  }

  async function captureScreenshot(filename = "android-shot.png", { display = true } = {}) {
    const result = await callAndroidTool(
      "android.capture_screenshot",
      {
        serial: state.currentSerial,
        filename,
      },
      {
        reason: "capture a fresh screenshot artifact for visual grounding",
      },
    );
    state.lastScreenshotPath = result.path ?? null;
    if (display && state.lastScreenshotPath && typeof displayScreenshot === "function") {
      await emitScreenshot(state.lastScreenshotPath);
    }
    return result;
  }

  async function dumpUiHierarchy(filename = "android-ui.xml") {
    const result = await callAndroidTool(
      "android.dump_ui_hierarchy",
      {
        serial: state.currentSerial,
        filename,
      },
      {
        reason: "capture a raw hierarchy artifact for precise follow-up",
      },
    );
    state.lastUiDumpPath = result.path ?? null;
    return result;
  }

  async function snapshotVisualContext(
    label = "android-state",
    { includeUi = true, display = true } = {},
  ) {
    const captured =
      !state.lastScreenshotPath || (includeUi && !state.lastUiDumpPath)
        ? await runtime.captureState(label, { includeUi, display })
        : null;

    return {
      serial: state.currentSerial,
      screenshotPath: captured?.screenshotPath ?? state.lastScreenshotPath,
      uiDumpPath: includeUi ? captured?.uiDumpPath ?? state.lastUiDumpPath : null,
      lastScenarioResult: state.lastScenarioResult,
      captured,
    };
  }

  async function runExplorationStep(
    label,
    action,
    {
      captureBefore = false,
      captureAfter = true,
      includeUi = true,
      display = true,
    } = {},
  ) {
    if (typeof action !== "function") {
      throw new Error("runExplorationStep requires an async action(runtime) function");
    }

    const before = captureBefore
      ? await runtime.captureState(`${label}-before`, { includeUi, display })
      : await snapshotVisualContext(`${label}-resume`, { includeUi, display });
    const actionResult = await action(runtime);
    const after = captureAfter
      ? await runtime.captureState(`${label}-after`, { includeUi, display })
      : await snapshotVisualContext(`${label}-after`, { includeUi, display });

    return {
      label,
      before,
      actionResult,
      after,
    };
  }

  const runtime = {
    async setSerial(serial) {
      state.currentSerial = serial;
      return state.currentSerial;
    },

    getState() {
      return {
        ...state,
        loadedToolGroups: [...state.loadedToolGroups],
      };
    },

    getToolLoadingPlan() {
      return cloneToolLoadingPlan();
    },

    async ensureToolGroupLoaded(groupName, reason = `prepare ${groupName}`) {
      return ensureToolGroupLoaded(groupName, reason);
    },

    async ensureBootstrapTools(reason = "prepare bootstrap Android tools") {
      return ensureToolGroupLoaded("bootstrap", reason);
    },

    async ensureObservationTools(reason = "prepare Android observation tools") {
      return ensureToolGroupLoaded("observation", reason);
    },

    async ensureSemanticUiTools(reason = "prepare Android semantic UI tools") {
      return ensureToolGroupLoaded("semanticUi", reason);
    },

    async ensureRawInputTools(reason = "prepare Android raw input tools") {
      return ensureToolGroupLoaded("rawInput", reason);
    },

    async ensureSolarLabTools(reason = "prepare Solar Lab scenario tools") {
      return ensureToolGroupLoaded("solarlab", reason);
    },

    async health() {
      return callAndroidTool("android.health", {}, {
        reason: "inspect Android harness health and environment",
      });
    },

    async listDevices() {
      return callAndroidTool("android.list_devices", {}, {
        reason: "discover current adb-visible devices",
      });
    },

    async listAvds() {
      return callAndroidTool("android.list_avds", {}, {
        reason: "discover installed AVDs before launch",
      });
    },

    async launchAvd({
      avdName,
      noWindow = false,
      gpu,
      grpcPort,
      extraArgs = [],
    }) {
      return callAndroidTool(
        "android.launch_avd",
        {
          avd_name: avdName,
          no_window: noWindow,
          gpu,
          grpc_port: grpcPort,
          extra_args: extraArgs,
        },
        {
          reason: "launch an Android emulator from the eager bootstrap lane",
        },
      );
    },

    async waitForBoot(timeoutSecs = 180) {
      return callAndroidTool(
        "android.wait_for_boot",
        {
          serial: state.currentSerial,
          timeout_secs: timeoutSecs,
        },
        {
          reason: "wait for device readiness before app work",
        },
      );
    },

    async installApk(apkPath, { reinstall = true } = {}) {
      return callAndroidTool(
        "android.install_apk",
        {
          serial: state.currentSerial,
          apk_path: apkPath,
          reinstall,
        },
        {
          reason: "install or refresh the APK before launching the app",
        },
      );
    },

    async launchApp(
      packageName,
      {
        activity,
        waitForSelector = null,
        waitForActivity = null,
        waitForPackage = null,
        timeoutSecs = 5,
      } = {},
    ) {
      const result = await callAndroidTool(
        "android.launch_app",
        {
          serial: state.currentSerial,
          package_name: packageName,
          activity,
          wait_for_selector: waitForSelector,
          wait_for_activity: waitForActivity,
          wait_for_package: waitForPackage,
          timeout_secs: timeoutSecs,
        },
        {
          reason: "enter the target app before observation or interaction",
        },
      );
      return throwIfNotOk(result, "launchApp did not satisfy the requested postcondition");
    },

    async captureScreenshot(label = "android-shot") {
      return captureScreenshot(`${label}.png`);
    },

    async captureState(
      label = "android-state",
      { includeUi = true, display = true } = {},
    ) {
      const screenshot = await captureScreenshot(`${label}.png`, { display });
      const ui = includeUi ? await dumpUiHierarchy(`${label}.xml`) : null;
      return {
        serial: state.currentSerial,
        screenshotPath: screenshot.path ?? null,
        uiDumpPath: ui?.path ?? null,
      };
    },

    async dumpUiHierarchy(label = "android-ui") {
      const result = await dumpUiHierarchy(`${label}.xml`);
      state.lastUiDumpPath = result.path ?? state.lastUiDumpPath;
      return result;
    },

    async inspectUi({
      hierarchyFilename = "inspect-ui.xml",
      includeScreenshot = true,
      screenshotFilename = "inspect-ui.png",
    } = {}) {
      const result = await callAndroidTool(
        "android.inspect_ui",
        {
          serial: state.currentSerial,
          hierarchy_filename: hierarchyFilename,
          include_screenshot: includeScreenshot,
          screenshot_filename: screenshotFilename,
        },
        {
          reason: "observe current Android state with paired UI and screenshot artifacts",
        },
      );
      state.lastUiDumpPath = result?.artifacts?.hierarchy_path ?? state.lastUiDumpPath;
      state.lastScreenshotPath =
        result?.artifacts?.screenshot_path ?? state.lastScreenshotPath;
      return result;
    },

    async waitForStableUi({
      timeoutSecs = 15,
      pollIntervalMs = 500,
      stablePolls = 2,
      hierarchyFilename = "wait-stable-ui.xml",
      includeScreenshot = true,
      screenshotFilename = "wait-stable-ui.png",
    } = {}) {
      const result = await callAndroidTool(
        "android.wait_for_stable_ui",
        {
          serial: state.currentSerial,
          timeout_secs: timeoutSecs,
          poll_interval_ms: pollIntervalMs,
          stable_polls: stablePolls,
          hierarchy_filename: hierarchyFilename,
          include_screenshot: includeScreenshot,
          screenshot_filename: screenshotFilename,
        },
        {
          reason: "wait for the UI to settle before semantic interaction",
        },
      );
      state.lastUiDumpPath = result?.artifacts?.hierarchy_path ?? state.lastUiDumpPath;
      state.lastScreenshotPath =
        result?.artifacts?.screenshot_path ?? state.lastScreenshotPath;
      return result;
    },

    async findUiElement(selector, { hierarchyFilename = "find-ui-element.xml" } = {}) {
      const result = await callAndroidTool(
        "android.find_ui_element",
        {
          serial: state.currentSerial,
          selector,
          hierarchy_filename: hierarchyFilename,
        },
        {
          reason: "resolve a semantic UI element before acting",
        },
      );
      state.lastUiDumpPath = result?.artifacts?.hierarchy_path ?? state.lastUiDumpPath;
      return throwIfNotOk(result, "findUiElement did not resolve a unique selector");
    },

    async waitForUiElement(
      selector,
      { timeoutSecs = 15, absent = false, hierarchyFilename = "wait-ui-element.xml" } = {},
    ) {
      const result = await callAndroidTool(
        "android.wait_for_ui_element",
        {
          serial: state.currentSerial,
          selector,
          timeout_secs: timeoutSecs,
          absent,
          hierarchy_filename: hierarchyFilename,
        },
        {
          reason: "wait for a semantic selector transition rather than polling manually",
        },
      );
      state.lastUiDumpPath = result?.artifacts?.hierarchy_path ?? state.lastUiDumpPath;
      return throwIfNotOk(result, "waitForUiElement did not satisfy selector");
    },

    async tapElement(
      selector,
      {
        hierarchyFilename = "tap-element.xml",
        waitUntilAbsent = false,
        waitForSelector = null,
        timeoutSecs = 5,
        retryWithAdbOnNoChange = true,
        allowVerificationFailure = false,
        matchIndex = null,
      } = {},
    ) {
      const result = await callAndroidTool(
        "android.tap_element",
        {
          serial: state.currentSerial,
          selector,
          hierarchy_filename: hierarchyFilename,
          wait_until_absent: waitUntilAbsent,
          wait_for_selector: waitForSelector,
          timeout_secs: timeoutSecs,
          retry_with_adb_on_no_change: retryWithAdbOnNoChange,
          allow_verification_failure: allowVerificationFailure,
          match_index: matchIndex,
        },
        {
          reason: "perform a semantic tap with verification instead of raw coordinates",
        },
      );
      state.lastUiDumpPath =
        result?.artifacts?.post_tap_hierarchy_path ??
        result?.artifacts?.hierarchy_path ??
        state.lastUiDumpPath;
      return throwIfNotOk(result, "tapElement did not verify the requested action");
    },

    async typeIntoElement(
      selector,
      text,
      { hierarchyFilename = "type-into-element.xml", timeoutSecs = 5, matchIndex = null } = {},
    ) {
      const result = await callAndroidTool(
        "android.type_into_element",
        {
          serial: state.currentSerial,
          selector,
          text,
          hierarchy_filename: hierarchyFilename,
          timeout_secs: timeoutSecs,
          match_index: matchIndex,
        },
        {
          reason: "perform semantic text entry with focus and text verification",
        },
      );
      state.lastUiDumpPath =
        result?.artifacts?.stable_hierarchy_path ??
        result?.artifacts?.hierarchy_path ??
        state.lastUiDumpPath;
      return throwIfNotOk(result, "typeIntoElement did not verify the requested action");
    },

    async scrollUntilVisible(
      selector,
      {
        direction = "down",
        maxSwipes = 5,
        hierarchyFilename = "scroll-until-visible.xml",
        matchIndex = null,
      } = {},
    ) {
      const result = await callAndroidTool(
        "android.scroll_until_visible",
        {
          serial: state.currentSerial,
          selector,
          direction,
          max_swipes: maxSwipes,
          hierarchy_filename: hierarchyFilename,
          match_index: matchIndex,
        },
        {
          reason: "use semantic scrolling before falling back to raw swipe coordinates",
        },
      );
      state.lastUiDumpPath = result?.artifacts?.hierarchy_path ?? state.lastUiDumpPath;
      return throwIfNotOk(result, "scrollUntilVisible did not resolve a unique visible target");
    },

    async collectLogcat({ filename = "android-logcat.txt", lines = 400 } = {}) {
      return callAndroidTool(
        "android.collect_logcat",
        {
          serial: state.currentSerial,
          filename,
          lines,
        },
        {
          reason: "capture logs after a failure or for a final proof bundle",
        },
      );
    },

    async tap(
      x,
      y,
      {
        tappedSelector = null,
        waitUntilAbsent = false,
        waitForSelector = null,
        timeoutSecs = 5,
        retryWithAdbOnNoChange = true,
      } = {},
    ) {
      const result = await callAndroidTool(
        "android.input.tap",
        {
          serial: state.currentSerial,
          x,
          y,
          tapped_selector: tappedSelector,
          wait_until_absent: waitUntilAbsent,
          wait_for_selector: waitForSelector,
          timeout_secs: timeoutSecs,
          retry_with_adb_on_no_change: retryWithAdbOnNoChange,
        },
        {
          reason: "fall back to a raw coordinate tap only when semantic interaction is insufficient",
        },
      );
      return throwIfNotOk(result, "tap did not satisfy the requested postcondition");
    },

    async typeText(
      text,
      { expectFocusSelector = null, waitForSelector = null, timeoutSecs = 5 } = {},
    ) {
      const result = await callAndroidTool(
        "android.input.text",
        {
          serial: state.currentSerial,
          text,
          expect_focus_selector: expectFocusSelector,
          wait_for_selector: waitForSelector,
          timeout_secs: timeoutSecs,
        },
        {
          reason: "fall back to raw text input when semantic field targeting is not the right fit",
        },
      );
      return throwIfNotOk(result, "typeText did not satisfy the requested postcondition");
    },

    async swipe(
      x1,
      y1,
      x2,
      y2,
      { durationMs, waitForSelector = null, expectScrollChange = false, timeoutSecs = 5 } = {},
    ) {
      const result = await callAndroidTool(
        "android.input.swipe",
        {
          serial: state.currentSerial,
          x1,
          y1,
          x2,
          y2,
          duration_ms: durationMs,
          wait_for_selector: waitForSelector,
          expect_scroll_change: expectScrollChange,
          timeout_secs: timeoutSecs,
        },
        {
          reason: "use a raw swipe only when semantic scrolling or stable selectors are not enough",
        },
      );
      return throwIfNotOk(result, "swipe did not satisfy the requested postcondition");
    },

    async keyevent(
      keycode,
      { waitForSelector = null, waitForActivity = null, waitForPackage = null, timeoutSecs = 5 } = {},
    ) {
      const result = await callAndroidTool(
        "android.input.keyevent",
        {
          serial: state.currentSerial,
          keycode,
          wait_for_selector: waitForSelector,
          wait_for_activity: waitForActivity,
          wait_for_package: waitForPackage,
          timeout_secs: timeoutSecs,
        },
        {
          reason: "send a bounded low-level key event as a fallback action",
        },
      );
      return throwIfNotOk(result, "keyevent did not satisfy the requested postcondition");
    },

    async runSolarLabScenario(name, options = {}) {
      const result = await callAndroidTool(
        `solarlab.scenario.${name}`,
        {
          ...options,
          serial: state.currentSerial,
        },
        {
          reason: "load and execute a Solar Lab-specific scenario on demand",
        },
      );
      state.lastScenarioResult = result;
      return result;
    },

    async displayLastScreenshot() {
      if (!state.lastScreenshotPath) {
        throw new Error("no screenshot has been captured in this runtime yet");
      }
      return emitScreenshot(state.lastScreenshotPath);
    },

    async displayScreenshot(path) {
      return emitScreenshot(path);
    },

    async getVisualContext({ includeUi = true } = {}) {
      return snapshotVisualContext("android-context", { includeUi, display: false });
    },

    async ensureVisualContext(
      label = "android-resume",
      { includeUi = true, display = true } = {},
    ) {
      return snapshotVisualContext(label, { includeUi, display });
    },

    async runExplorationStep(label, action, options = {}) {
      return runExplorationStep(label, action, options);
    },
  };

  return runtime;
}
