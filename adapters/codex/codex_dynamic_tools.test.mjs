import test from "node:test";
import assert from "node:assert/strict";

import { createCodexAndroidDynamicToolHost } from "./codex_dynamic_tools.js";

function createRuntime(overrides = {}) {
  const calls = [];
  const runtime = {
    calls,
    currentSerial: null,
    getState() {
      return {
        currentSerial: this.currentSerial,
      };
    },
    async listDevices() {
      calls.push({ fn: "listDevices" });
      return {
        ok: true,
        devices: [{ serial: "emulator-5554" }],
      };
    },
    async setSerial(serial) {
      calls.push({ fn: "setSerial", serial });
      this.currentSerial = serial;
      return serial;
    },
    async captureScreenshot(label) {
      calls.push({ fn: "captureScreenshot", label });
      return { path: `/tmp/${label}.png` };
    },
    async captureState(label, { includeUi = true } = {}) {
      calls.push({ fn: "captureState", label, includeUi });
      return {
        serial: this.currentSerial,
        screenshotPath: `/tmp/${label}.png`,
        uiDumpPath: includeUi ? `/tmp/${label}.xml` : null,
      };
    },
    async displayScreenshot(filePath) {
      calls.push({ fn: "displayScreenshot", filePath });
      return filePath;
    },
    async waitForStableUi({ hierarchyFilename, screenshotFilename }) {
      calls.push({
        fn: "waitForStableUi",
        hierarchyFilename,
        screenshotFilename,
      });
      return {
        ok: true,
        serial: this.currentSerial,
        artifacts: {
          hierarchy_path: `/tmp/${hierarchyFilename}`,
          screenshot_path: `/tmp/${screenshotFilename}`,
        },
        window_state: {
          input_method_visible: false,
          input_method_target: null,
        },
      };
    },
    async waitForUiElement(selector, options) {
      calls.push({ fn: "waitForUiElement", selector, options });
      return {
        ok: true,
        satisfied: true,
        postcondition: { satisfied: true },
      };
    },
    async launchApp(packageName, options) {
      calls.push({ fn: "launchApp", packageName, options });
      return {
        ok: true,
        observed_activity: options.activity ?? ".MainActivity",
        observed_package: packageName,
        postcondition: { satisfied: true },
      };
    },
    async tapElement(selector, options) {
      calls.push({ fn: "tapElement", selector, options });
      return {
        ok: true,
        postcondition: { satisfied: true },
      };
    },
    async tap(x, y, options) {
      calls.push({ fn: "tap", x, y, options });
      return {
        ok: true,
        postcondition: { satisfied: true },
      };
    },
    async doubleTap(x, y, options) {
      calls.push({ fn: "doubleTap", x, y, options });
      return {
        ok: true,
        postcondition: { satisfied: true },
      };
    },
    async longPress(x, y, options) {
      calls.push({ fn: "longPress", x, y, options });
      return {
        ok: true,
        postcondition: { satisfied: true },
      };
    },
    async openUrl(url, options) {
      calls.push({ fn: "openUrl", url, options });
      return {
        ok: true,
        postcondition: { satisfied: true },
      };
    },
    async setOrientation(orientation, options) {
      calls.push({ fn: "setOrientation", orientation, options });
      return {
        ok: true,
        postcondition: { satisfied: true },
      };
    },
    async typeIntoElement(selector, text, options) {
      calls.push({ fn: "typeIntoElement", selector, text, options });
      return {
        ok: true,
        postcondition: { satisfied: true },
      };
    },
    async typeText(text, options) {
      calls.push({ fn: "typeText", text, options });
      return {
        ok: true,
        postcondition: { satisfied: true },
      };
    },
    async keyevent(keycode, options) {
      calls.push({ fn: "keyevent", keycode, options });
      return {
        ok: true,
        postcondition: { satisfied: true },
      };
    },
    async keyCombination(keycodes, options) {
      calls.push({ fn: "keyCombination", keycodes, options });
      return {
        ok: true,
        postcondition: { satisfied: true },
      };
    },
    async swipe(x1, y1, x2, y2, options) {
      calls.push({ fn: "swipe", x1, y1, x2, y2, options });
      return {
        ok: true,
        postcondition: { satisfied: true },
      };
    },
    async multiTouch(pointers, options) {
      calls.push({ fn: "multiTouch", pointers, options });
      return {
        ok: true,
        capability: { status: "supported" },
      };
    },
    async semanticAction(name, options) {
      calls.push({ fn: "semanticAction", name, options });
      return {
        ok: true,
        postcondition: { satisfied: true },
      };
    },
    ...overrides,
  };
  return runtime;
}

test("Codex Android dynamic tool host exposes native observe and step tools", async () => {
  const host = createCodexAndroidDynamicToolHost({
    runtime: createRuntime(),
    readFile: async (filePath) => {
      if (filePath.endsWith(".xml")) {
        return `
          <hierarchy>
            <node text="" content-desc="" resource-id="" scrollable="true" bounds="[0,0][1080,2400]">
              <node text="Search" content-desc="Search" resource-id="com.example:id/search" bounds="[48,96][240,180]"/>
              <node text="Frame" content-desc="" resource-id="com.example:id/frame" enabled="false" bounds="[300,96][520,180]"/>
              <node text="Mission feed" content-desc="" resource-id="com.example:id/feed" scrollable="true" bounds="[48,220][720,420]"/>
              <node text="Advance" content-desc="" resource-id="com.example:id/advance" bounds="[1040,300][1120,360]"/>
            </node>
          </hierarchy>
        `;
      }
      return Buffer.from(`bytes:${filePath}`);
    },
  });

  const specs = host.getToolSpecs();
  assert.deepEqual(
    specs.map((spec) => spec.name),
    ["android_observe", "android_step", "android_install_build_from_run"],
  );
  assert.deepEqual(
    specs.map((spec) => spec.persistOnResume),
    [false, false, false],
  );
  const stepSpec = specs.find((spec) => spec.name === "android_step");
  assert.equal(stepSpec.inputSchema.properties.action.enum.includes("multi_touch"), true);
  assert.equal(stepSpec.inputSchema.properties.pointers.minItems, 2);
  assert.equal(stepSpec.inputSchema.properties.pointers.maxItems, 5);
  assert.deepEqual(
    specs.map((spec) => spec.capability),
    [
      {
        family: "android",
        capabilityScope: "environment",
        mutationClass: "read_only",
        leaseMode: "shared_read",
      },
      {
        family: "android",
        capabilityScope: "environment",
        mutationClass: "mutating",
        leaseMode: "exclusive_write",
      },
      {
        family: "android",
        capabilityScope: "environment",
        mutationClass: "mutating",
        leaseMode: "exclusive_write",
      },
    ],
  );

  const response = await host.executeToolCall({
    tool: "android_observe",
    arguments: {
      prompt: "Focus on the visible controls",
      scope: "screen_and_ui",
    },
  });

  assert.equal(response.success, true);
  assert.equal(response.contentItems[0].type, "inputText");
  assert.match(response.contentItems[0].text, /Android observation/);
  assert.match(response.contentItems[0].text, /outcome: succeeded/);
  assert.equal(response.metadata.android.outcome.status, "succeeded");
  assert.match(response.contentItems[0].text, /ui digest:/);
  assert.match(response.contentItems[0].text, /visible_ui: Search \[48,96\]\[240,180\]/);
  assert.match(response.contentItems[0].text, /Frame \[disabled\] \[300,96\]\[520,180\]/);
  assert.match(response.contentItems[0].text, /Mission feed \[scrollable\] \[48,220\]\[720,420\]/);
  assert.match(response.contentItems[0].text, /Advance \[clipped right 50%\] \[1040,300\]\[1120,360\]/);
  assert.match(response.contentItems[0].text, /clipped_nodes: 1/);
  assert.match(response.contentItems[0].text, /scrollable_nodes: 2/);
  assert.doesNotMatch(response.contentItems[0].text, /soft_keyboard:/);
  assert.equal(response.contentItems[1].type, "inputImage");
  assert.equal(response.contentItems[1].detail, "original");
});

test("android_install_build_from_run installs provider-side build and returns fresh visual context", async () => {
  const runtime = createRuntime({
    async installBuildFromRun(args) {
      this.calls.push({ fn: "installBuildFromRun", args });
      this.currentSerial = args.serial ?? this.currentSerial ?? "emulator-5554";
      return {
        ok: true,
        serial: this.currentSerial,
        installed: true,
        manifest: {
          run_id: String(args.workflow_run_id),
          artifact_name: args.artifact_name,
          checkout_ref: "feature-branch",
          commit_sha: "acedb057b55387fe121fa82ca2e4af67d98741d0",
          version_name: "0.1.1-alpha.2",
          package_name: "com.sednalabs.solarlab",
          activity_name: ".MainActivity",
          android_validation_mode: "stage-first-mirror-on",
          interactive_debug_profile: "hosted-debug-lite",
        },
        postcondition: {
          satisfied: true,
        },
      };
    },
  });
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => {
      if (filePath.endsWith(".xml")) {
        return `
          <hierarchy>
            <node text="Sandbox" content-desc="" resource-id="com.sednalabs.solarlab:id/sandbox" bounds="[48,96][240,180]"/>
          </hierarchy>
        `;
      }
      return Buffer.from(`bytes:${filePath}`);
    },
  });

  const response = await host.executeToolCall({
    tool: "android_install_build_from_run",
    arguments: {
      workflow_run_id: 25106447821,
      artifact_name: "interactive-android-build-stage-first-mirror-on-hosted-debug-lite",
      post_observe_scope: "screen_and_ui",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[0].fn, "installBuildFromRun");
  assert.deepEqual(runtime.calls[0].args, {
    workflow_run_id: 25106447821,
    artifact_name: "interactive-android-build-stage-first-mirror-on-hosted-debug-lite",
    repository: null,
    launch_after_install: true,
    serial: null,
    timeout_secs: null,
  });
  assert.equal(runtime.calls[1].fn, "waitForStableUi");
  assert.match(response.contentItems[0].text, /Android build install/);
  assert.match(response.contentItems[0].text, /workflow_run_id: 25106447821/);
  assert.match(response.contentItems[0].text, /postcondition satisfied: true/);
  assert.match(response.contentItems[0].text, /visible_ui: Sandbox/);
  assert.equal(response.contentItems[1].type, "inputImage");
  assert.equal(response.metadata.android.outcome.status, "succeeded");
});

test("android_observe reports visible soft keyboard from window state", async () => {
  const runtime = createRuntime({
    async waitForStableUi({ hierarchyFilename, screenshotFilename }) {
      return {
        ok: true,
        serial: "emulator-5554",
        artifacts: {
          hierarchy_path: `/tmp/${hierarchyFilename}`,
          screenshot_path: `/tmp/${screenshotFilename}`,
        },
        window_state: {
          input_method_visible: true,
          input_method_target: "com.example/.MainActivity",
        },
      };
    },
  });
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async () => "<hierarchy />",
  });

  const response = await host.executeToolCall({
    tool: "android_observe",
    arguments: { scope: "screen_and_ui" },
  });

  assert.match(
    response.contentItems[0].text,
    /soft_keyboard: visible for com\.example\/\.MainActivity/,
  );
});

test("android_step prefers semantic actions when selectors are provided", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    defaultPackageName: "com.sednalabs.solarlab",
    defaultActivity: ".MainActivity",
    readFile: async (filePath) => {
      if (filePath.endsWith(".xml")) {
        return `
          <hierarchy>
            <node text="Sandbox" content-desc="" resource-id="com.sednalabs.solarlab:id/sandbox"/>
          </hierarchy>
        `;
      }
      return Buffer.from(`bytes:${filePath}`);
    },
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      action: "tap",
      selector: { text: "Sandbox" },
      post_observe_scope: "screen_and_ui",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[0].fn, "listDevices");
  assert.equal(runtime.calls[2].fn, "tapElement");
  assert.equal(runtime.calls[3].fn, "waitForStableUi");
  assert.equal(response.contentItems[1].type, "inputImage");
  assert.equal(response.contentItems[1].detail, "original");
});

test("android_step upgrades bare string tap selectors into exact interactive labels", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      action: "tap",
      selector: "Open immersive view",
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[2].fn, "tapElement");
  assert.deepEqual(runtime.calls[2].selector, {
    label: "Open immersive view",
    label_exact: true,
  });
  assert.match(response.contentItems[0].text, /actions executed: 1/);
  assert.equal(response.metadata.android.outcome.status, "succeeded");
});

test("android_step treats target as a semantic tap selector", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      action: "tap",
      target: "Search",
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[2].fn, "tapElement");
  assert.deepEqual(runtime.calls[2].selector, {
    label: "Search",
    label_exact: true,
  });
  assert.match(response.contentItems[0].text, /selector: "Search"/);
  assert.equal(response.metadata.android.outcome.status, "succeeded");
});

test("android_step selector type uses the provider's verified semantic text entry", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      action: "type_text",
      selector: { text: "Search by name or id" },
      text: "Earth",
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[2].fn, "typeIntoElement");
  assert.deepEqual(runtime.calls[2].selector, { text: "Search by name or id" });
  assert.equal(runtime.calls[2].text, "Earth");
  assert.deepEqual(runtime.calls[2].options, {
    timeoutSecs: 5,
    matchIndex: null,
  });
  assert.match(response.contentItems[0].text, /actions executed: 1/);
});

test("android_step normalizes selector aliases before semantic interaction", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      action: "tap",
      selector: {
        content_description: "Camera scale Close; open camera controls",
        resourceId: "com.example:id/camera",
      },
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[2].fn, "tapElement");
  assert.deepEqual(runtime.calls[2].selector, {
    content_desc: "Camera scale Close; open camera controls",
    resource_id: "com.example:id/camera",
  });
});

test("android_step taps the center of a UI-tree bounds selector", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      action: "tap",
      selector: {
        bounds: { left: 546, top: 2161, right: 682, bottom: 2287 },
      },
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[2].fn, "tap");
  assert.equal(runtime.calls[2].x, 614);
  assert.equal(runtime.calls[2].y, 2224);
});

test("android_step waits separately when a post-tap selector must disappear", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      action: "tap",
      selector: { text: "Got it" },
      wait_for_selector: { text: "Got it" },
      wait_until_absent: true,
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[2].fn, "tapElement");
  assert.equal(runtime.calls[2].options.waitForSelector, null);
  assert.equal(runtime.calls[2].options.waitUntilAbsent, false);
  assert.equal(runtime.calls[3].fn, "waitForUiElement");
  assert.deepEqual(runtime.calls[3].selector, { text: "Got it" });
  assert.deepEqual(runtime.calls[3].options, { timeoutSecs: 5, absent: true });
});

test("android_step maps a focus_body target to the provider body_query contract", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      action: "semantic_action",
      action_name: "focus_body",
      target: "comet",
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[2].fn, "semanticAction");
  assert.equal(runtime.calls[2].name, "focus_body");
  assert.deepEqual(runtime.calls[2].options, {
    target: "comet",
    bodyQuery: "comet",
    timeout_secs: 5,
  });
});

test("android_step maps selector-shaped focus_body aliases to the provider body_query contract", async () => {
  const targets = [
    { text: "comet" },
    { text_exact: "comet" },
    { label: "comet" },
    { label_exact: "comet" },
    { content_desc: "comet" },
    { content_description: "comet" },
    { contentDescription: "comet" },
  ];

  for (const target of targets) {
    const runtime = createRuntime();
    const host = createCodexAndroidDynamicToolHost({
      runtime,
      readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
    });

    const response = await host.executeToolCall({
      tool: "android_step",
      arguments: {
        actions: [
          {
            type: "semantic_action",
            action_name: "focus_body",
            target,
          },
        ],
        post_observe_scope: "screen",
      },
    });

    assert.equal(response.success, true);
    assert.deepEqual(runtime.calls[2], {
      fn: "semanticAction",
      name: "focus_body",
      options: {
        target,
        bodyQuery: "comet",
        timeout_secs: 5,
      },
    });
  }
});

test("android_step accepts batched computer-style actions and reports view metadata", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => {
      if (filePath.endsWith(".xml")) {
        return `
          <hierarchy>
            <node text="Earth" content-desc="Earth" resource-id="com.sednalabs.solarlab:id/earth"/>
          </hierarchy>
        `;
      }
      return Buffer.from(`bytes:${filePath}`);
    },
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      actions: [
        {
          type: "zoom",
          region: { left: 100, top: 200, width: 400, height: 800 },
        },
        {
          type: "click",
          x: 250,
          y: 500,
        },
        {
          type: "reset_zoom",
        },
      ],
      view: {
        device_width: 1000,
        device_height: 2000,
        frame_width: 500,
        frame_height: 1000,
      },
      post_observe_scope: "screen_and_ui",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[0].fn, "listDevices");
  assert.equal(runtime.calls[2].fn, "tap");
  assert.equal(runtime.calls[2].x, 300);
  assert.equal(runtime.calls[2].y, 600);
  assert.deepEqual(runtime.calls[2].options, {
    timeoutSecs: 5,
    waitForSelector: null,
    waitUntilAbsent: false,
    matchIndex: null,
  });
  assert.equal(runtime.calls[3].fn, "captureState");
  assert.match(response.contentItems[0].text, /actions executed: 3/);
  assert.match(response.contentItems[0].text, /"zoomed":false/);
  assert.equal(response.contentItems[1].type, "inputImage");
  assert.equal(response.contentItems[1].detail, "original");
});

test("android_step reports bridge-backed action failures as structured outcomes", async () => {
  const runtime = createRuntime({
    async tap(x, y, options) {
      this.calls.push({ fn: "tap", x, y, options });
      return {
        ok: false,
        note: "tap target did not settle",
      };
    },
  });
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      actions: [
        {
          type: "click",
          x: 250,
          y: 500,
        },
      ],
      view: {
        device_width: 1000,
        device_height: 2000,
        frame_width: 500,
        frame_height: 1000,
      },
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, false);
  assert.equal(runtime.calls[2].fn, "tap");
  assert.equal(response.metadata.android.outcome.status, "postcondition_failed");
  assert.equal(response.metadata.android.outcome.retryability, "observe_then_retry");
  assert.match(response.contentItems[0].text, /outcome: postcondition_failed/);
});

test("android_step reports later bridge-backed batch failures as structured outcomes", async () => {
  let tapCount = 0;
  const runtime = createRuntime({
    async tap(x, y, options) {
      tapCount += 1;
      this.calls.push({ fn: "tap", x, y, options });
      return tapCount === 1
        ? { ok: true, postcondition: { satisfied: true } }
        : { ok: false, note: "second tap failed" };
    },
  });
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      actions: [
        { type: "click", x: 100, y: 100 },
        { type: "click", x: 250, y: 500 },
      ],
      view: {
        device_width: 1000,
        device_height: 2000,
        frame_width: 500,
        frame_height: 1000,
      },
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, false);
  assert.equal(runtime.calls.filter((call) => call.fn === "tap").length, 2);
  assert.equal(response.metadata.android.outcome.status, "postcondition_failed");
  assert.match(response.contentItems[0].text, /outcome: postcondition_failed/);
});

test("android_step preserves wait options through bridge-backed click and type actions", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      actions: [
        {
          type: "click",
          x: 250,
          y: 500,
          timeout_secs: 9,
          wait_for_selector: { text: "Ready" },
          wait_until_absent: true,
          match_index: 2,
        },
        {
          type: "type",
          text: "Earth",
          timeout_secs: 7,
          wait_for_selector: { text: "Earth" },
          expect_focus_selector: { focused: true },
        },
      ],
      view: {
        device_width: 1000,
        device_height: 2000,
        frame_width: 500,
        frame_height: 1000,
      },
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[2].fn, "tap");
  assert.deepEqual(runtime.calls[2].options, {
    timeoutSecs: 9,
    waitForSelector: { text: "Ready" },
    waitUntilAbsent: true,
    matchIndex: 2,
  });
  assert.equal(runtime.calls[3].fn, "typeText");
  assert.deepEqual(runtime.calls[3].options, {
    timeoutSecs: 7,
    waitForSelector: { text: "Earth" },
    expectFocusSelector: { focused: true },
  });
});

test("android_step preserves wait options through bridge-backed printable keypress actions", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      actions: [
        {
          type: "keypress",
          keys: ["a"],
          timeout_secs: 6,
          wait_for_selector: { text: "Typed" },
          expect_focus_selector: { focused: true },
        },
      ],
      view: {
        device_width: 1000,
        device_height: 2000,
        frame_width: 500,
        frame_height: 1000,
      },
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[2].fn, "typeText");
  assert.equal(runtime.calls[2].text, "a");
  assert.deepEqual(runtime.calls[2].options, {
    timeoutSecs: 6,
    waitForSelector: { text: "Typed" },
    expectFocusSelector: { focused: true },
  });
});

test("android_step keeps legacy single-action args working alongside the new contract", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      action: "key",
      keycode: "KEYCODE_BACK",
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[2].fn, "keyevent");
  assert.equal(runtime.calls[2].keycode, "KEYCODE_BACK");
  assert.match(response.contentItems[0].text, /actions executed: 1/);
});

test("android_step dispatches double_click through the atomic runtime doubleTap path", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      actions: [{ type: "double_click", x: 120, y: 240, wait_for_selector: { text: "Ready" } }],
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[2].fn, "doubleTap");
  assert.equal(runtime.calls[2].x, 120);
  assert.equal(runtime.calls[2].y, 240);
  assert.deepEqual(runtime.calls[2].options.waitForSelector, { text: "Ready" });
  assert.match(response.contentItems[0].text, /actions executed: 1/);
});

test("android_step dispatches long_press through the runtime longPress path", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      actions: [{ type: "long_press", x: 120, y: 240, duration_ms: 850 }],
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[2].fn, "longPress");
  assert.equal(runtime.calls[2].x, 120);
  assert.equal(runtime.calls[2].y, 240);
  assert.equal(runtime.calls[2].options.durationMs, 850);
  assert.match(response.contentItems[0].text, /actions executed: 1/);
});

test("android_step dispatches every multi_touch pointer through one runtime call", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });
  const pointers = [
    { x1: 400, y1: 800, x2: 300, y2: 800 },
    { x1: 600, y1: 800, x2: 700, y2: 800 },
  ];

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      actions: [{ type: "multi_touch", pointers, duration_ms: 420, timeout_secs: 7 }],
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[2].fn, "multiTouch");
  assert.deepEqual(runtime.calls[2].pointers, pointers);
  assert.deepEqual(runtime.calls[2].options, { durationMs: 420, timeoutSecs: 7 });
  assert.match(response.contentItems[0].text, /actions executed: 1/);
});

test("android_step supports URL and orientation actions without making them app-admin tools", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      actions: [
        { type: "open_url", url: "https://example.test", wait_for_package: "com.android.chrome" },
        { type: "set_orientation", orientation: "landscape", timeout_secs: 8 },
      ],
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[2].fn, "openUrl");
  assert.equal(runtime.calls[2].url, "https://example.test");
  assert.equal(runtime.calls[2].options.waitForPackage, "com.android.chrome");
  assert.equal(runtime.calls[3].fn, "setOrientation");
  assert.equal(runtime.calls[3].orientation, "landscape");
  assert.equal(runtime.calls[3].options.timeoutSecs, 8);
  assert.match(response.contentItems[0].text, /actions executed: 2/);
});

test("android_step reports stale view geometry as a structured failure", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      actions: [
        { type: "set_orientation", orientation: "landscape" },
        { type: "click", x: 120, y: 240 },
      ],
      view: {
        device_width: 1000,
        device_height: 2000,
        frame_width: 500,
        frame_height: 1000,
      },
    },
  });

  assert.equal(response.success, false);
  assert.match(response.contentItems[0].text, /outcome: stale_view/);
  assert.equal(response.metadata.android.outcome.status, "stale_view");
  assert.equal(response.metadata.android.outcome.retryability, "observe_then_retry");
  assert.equal(runtime.calls[2].fn, "setOrientation");
});

test("android_step dispatches modifier keypresses as one key combination", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      actions: [{ type: "keypress", keys: ["Ctrl", "KEYCODE_C"], wait_for_package: "com.example" }],
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[2].fn, "keyCombination");
  assert.deepEqual(runtime.calls[2].keycodes, ["CTRL_LEFT", "C"]);
  assert.equal(runtime.calls[2].options.waitForPackage, "com.example");
  assert.match(response.contentItems[0].text, /actions executed: 1/);
});

test("android_step batched launch_app honors top-level package and activity defaults", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      actions: [{ type: "launch_app" }],
      package_name: "com.sednalabs.solarlab",
      activity: ".SandboxActivity",
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[2].fn, "launchApp");
  assert.equal(runtime.calls[2].packageName, "com.sednalabs.solarlab");
  assert.equal(runtime.calls[2].options.activity, ".SandboxActivity");
  assert.match(response.contentItems[0].text, /actions executed: 1/);
});

test("android_step launch_app does not leak default activity into other packages", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    defaultPackageName: "com.sednalabs.solarlab",
    defaultActivity: ".MainActivity",
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      actions: [{ type: "launch_app", package_name: "com.android.settings" }],
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[2].fn, "launchApp");
  assert.equal(runtime.calls[2].packageName, "com.android.settings");
  assert.equal(runtime.calls[2].options.activity, null);
  assert.match(response.contentItems[0].text, /actions executed: 1/);
});

test("android_step keeps verified launch success when hierarchy observation degrades", async () => {
  const runtime = createRuntime({
    async launchApp(packageName, options) {
      this.calls.push({ fn: "launchApp", packageName, options });
      return {
        ok: true,
        observed_activity: "com.sednalabs.solarlab/.MainActivity",
        observed_package: packageName,
        postcondition: {
          satisfied: true,
          evidence_source: "window_state",
        },
      };
    },
    async waitForStableUi({ hierarchyFilename, screenshotFilename }) {
      this.calls.push({ fn: "waitForStableUi", hierarchyFilename, screenshotFilename });
      throw new Error("UI hierarchy dump timed out");
    },
  });
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      action: "launch_app",
      package_name: "com.sednalabs.solarlab",
      activity: ".MainActivity",
      post_observe_scope: "screen_and_ui",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[2].fn, "launchApp");
  assert.equal(runtime.calls[3].fn, "waitForStableUi");
  assert.equal(runtime.calls[4].fn, "captureScreenshot");
  assert.match(response.contentItems[0].text, /outcome: observe_degraded/);
  assert.match(response.contentItems[0].text, /retryability: observe_then_retry/);
  assert.match(response.contentItems[0].text, /activity: com\.sednalabs\.solarlab\/\.MainActivity/);
  assert.match(response.contentItems[0].text, /package: com\.sednalabs\.solarlab/);
  assert.equal(response.metadata.android.outcome.status, "observe_degraded");
  assert.equal(response.metadata.android.outcome.postconditionSatisfied, true);
  assert.equal(response.contentItems[1].type, "inputImage");
});

test("android_step treats launch observation degradation without screenshot fallback as non-success", async () => {
  const runtime = createRuntime({
    async waitForStableUi({ hierarchyFilename, screenshotFilename }) {
      this.calls.push({ fn: "waitForStableUi", hierarchyFilename, screenshotFilename });
      throw new Error("UI hierarchy dump timed out");
    },
    async captureScreenshot(label) {
      this.calls.push({ fn: "captureScreenshot", label });
      return { ok: true };
    },
  });
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      action: "launch_app",
      package_name: "com.sednalabs.solarlab",
      post_observe_scope: "screen_and_ui",
    },
  });

  assert.equal(response.success, false);
  assert.equal(response.contentItems.length, 1);
  assert.match(response.contentItems[0].text, /actions executed: 1/);
  assert.match(response.contentItems[0].text, /outcome: observe_degraded/);
  assert.match(response.contentItems[0].text, /missing native image output/);
  assert.equal(response.metadata.android.outcome.status, "observe_degraded");
  assert.equal(response.metadata.android.outcome.postconditionSatisfied, true);
});

test("android_step legacy launch_app keeps explicit package separate from batch defaults", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    defaultPackageName: "com.sednalabs.solarlab",
    defaultActivity: ".MainActivity",
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      action: "launch_app",
      package_name: "com.android.settings",
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, true);
  assert.equal(runtime.calls[2].fn, "launchApp");
  assert.equal(runtime.calls[2].packageName, "com.android.settings");
  assert.equal(runtime.calls[2].options.activity, null);
  assert.match(response.contentItems[0].text, /actions executed: 1/);
});

test("dynamic tool host prefers remote artifact reads for remote session paths", async () => {
  const remoteReads = [];
  const host = createCodexAndroidDynamicToolHost({
    runtime: createRuntime({
      async waitForStableUi({ hierarchyFilename, screenshotFilename }) {
        return {
          ok: true,
          serial: "emulator-5554",
          artifacts: {
            hierarchy_path: `dist/interactive-session/android-computer-use-mcp-artifacts/uiautomator/${hierarchyFilename}`,
            screenshot_path: `dist/interactive-session/android-computer-use-mcp-artifacts/screenshots/${screenshotFilename}`,
          },
        };
      },
    }),
    readFile: async () => {
      throw Object.assign(new Error("local file missing"), { code: "ENOENT" });
    },
    readRemoteArtifactFile: async (filePath, encoding) => {
      remoteReads.push({ filePath, encoding: encoding ?? null });
      if (filePath.endsWith(".xml")) {
        return `
          <hierarchy>
            <node text="Earth" content-desc="Earth" resource-id="com.sednalabs.solarlab:id/earth"/>
          </hierarchy>
        `;
      }
      return Buffer.from(`remote:${filePath}`);
    },
  });

  const response = await host.executeToolCall({
    tool: "android_observe",
    arguments: {
      scope: "screen_and_ui",
    },
  });

  assert.equal(response.success, true);
  assert.equal(response.contentItems[1].type, "inputImage");
  assert.deepEqual(
    remoteReads.map(({ filePath }) => filePath),
    [
      "dist/interactive-session/android-computer-use-mcp-artifacts/uiautomator/codex-android-observe.xml",
      "dist/interactive-session/android-computer-use-mcp-artifacts/screenshots/codex-android-observe.png",
    ],
  );
});

test("android_observe falls back to screenshot-only output when ui dump fails", async () => {
  const host = createCodexAndroidDynamicToolHost({
    runtime: createRuntime({
      async waitForStableUi() {
        throw new Error("uiautomator dump failed");
      },
    }),
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_observe",
    arguments: {
      scope: "screen_and_ui",
    },
  });

  assert.equal(response.success, true);
  assert.equal(response.contentItems[0].type, "inputText");
  assert.match(response.contentItems[0].text, /ui digest unavailable after observe/);
  assert.match(response.contentItems[0].text, /outcome: observe_degraded/);
  assert.equal(response.metadata.android.outcome.retryability, "observe_then_retry");
  assert.equal(response.contentItems[1].type, "inputImage");
  assert.equal(response.contentItems[1].detail, "original");
});

test("android_observe fails loudly when no native image can be attached", async () => {
  const host = createCodexAndroidDynamicToolHost({
    runtime: createRuntime({
      async captureScreenshot(label) {
        this.calls.push({ fn: "captureScreenshot", label });
        return { ok: true };
      },
    }),
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_observe",
    arguments: {
      scope: "screen",
    },
  });

  assert.equal(response.success, false);
  assert.equal(response.contentItems.length, 1);
  assert.equal(response.contentItems[0].type, "inputText");
  assert.match(response.contentItems[0].text, /outcome: observe_degraded/);
  assert.match(response.contentItems[0].text, /missing native image output/);
  assert.equal(response.metadata.android.outcome.status, "observe_degraded");
  assert.equal(response.metadata.android.outcome.retryability, "observe_then_retry");
});

test("android_step reports post-action native image loss as non-success without hiding executed actions", async () => {
  const host = createCodexAndroidDynamicToolHost({
    runtime: createRuntime({
      async waitForStableUi({ hierarchyFilename, screenshotFilename }) {
        this.calls.push({
          fn: "waitForStableUi",
          hierarchyFilename,
          screenshotFilename,
        });
        return {
          ok: true,
          serial: this.currentSerial,
          artifacts: {
            hierarchy_path: `/tmp/${hierarchyFilename}`,
          },
        };
      },
    }),
    readFile: async (filePath) => {
      if (filePath.endsWith(".xml")) {
        return "<hierarchy />";
      }
      return Buffer.from(`bytes:${filePath}`);
    },
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      action: "tap",
      x: 10,
      y: 20,
    },
  });

  assert.equal(response.success, false);
  assert.equal(response.contentItems.length, 1);
  assert.equal(response.contentItems[0].type, "inputText");
  assert.match(response.contentItems[0].text, /actions executed: 1/);
  assert.match(response.contentItems[0].text, /outcome: observe_degraded/);
  assert.match(response.contentItems[0].text, /android_step post-action observation missing native image output/);
  assert.equal(response.metadata.android.outcome.status, "observe_degraded");
  assert.equal(response.metadata.android.outcome.actionTypes[0], "click");
});

test("android_install_build_from_run reports post-install native image loss as non-success", async () => {
  const host = createCodexAndroidDynamicToolHost({
    runtime: createRuntime({
      async installBuildFromRun(args) {
        this.calls.push({ fn: "installBuildFromRun", args });
        return {
          ok: true,
          serial: this.currentSerial,
          installed: true,
          postcondition: { satisfied: true },
        };
      },
      async waitForStableUi({ hierarchyFilename, screenshotFilename }) {
        this.calls.push({
          fn: "waitForStableUi",
          hierarchyFilename,
          screenshotFilename,
        });
        return {
          ok: true,
          serial: this.currentSerial,
          artifacts: {
            hierarchy_path: `/tmp/${hierarchyFilename}`,
          },
          postcondition: { satisfied: true },
        };
      },
    }),
    readFile: async (filePath) => {
      if (filePath.endsWith(".xml")) {
        return "<hierarchy />";
      }
      return Buffer.from(`bytes:${filePath}`);
    },
  });

  const response = await host.executeToolCall({
    tool: "android_install_build_from_run",
    arguments: {
      workflow_run_id: 25106447821,
      artifact_name: "interactive-android-build-stage-first-mirror-on-hosted-debug-lite",
    },
  });

  assert.equal(response.success, false);
  assert.equal(response.contentItems.length, 1);
  assert.equal(response.contentItems[0].type, "inputText");
  assert.match(response.contentItems[0].text, /outcome: observe_degraded/);
  assert.match(
    response.contentItems[0].text,
    /android_install_build_from_run post-install observation missing native image output/,
  );
  assert.equal(response.metadata.android.outcome.status, "observe_degraded");
  assert.equal(response.metadata.android.outcome.retryability, "observe_then_retry");
});

test("android_step reports postcondition failures as structured non-success outcomes", async () => {
  const host = createCodexAndroidDynamicToolHost({
    runtime: createRuntime({
      async tap() {
        return {
          ok: true,
          note: "selector did not appear",
          postcondition: { satisfied: false },
        };
      },
    }),
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      action: "tap",
      x: 10,
      y: 20,
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, false);
  assert.match(response.contentItems[0].text, /outcome: postcondition_failed/);
  assert.equal(response.metadata.android.outcome.status, "postcondition_failed");
  assert.equal(response.metadata.android.outcome.retryability, "observe_then_retry");
  assert.equal(response.metadata.android.outcome.postconditionSatisfied, false);
});

test("android_step converts thrown runtime failures into structured outcomes", async () => {
  const host = createCodexAndroidDynamicToolHost({
    runtime: createRuntime({
      async tap() {
        const error = new Error("tap did not report success: {\"ok\":false}");
        error.result = { ok: false };
        throw error;
      },
    }),
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      action: "tap",
      x: 10,
      y: 20,
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, false);
  assert.match(response.contentItems[0].text, /outcome: postcondition_failed/);
  assert.equal(response.metadata.android.outcome.status, "postcondition_failed");
  assert.equal(response.metadata.android.outcome.retryability, "observe_then_retry");
});

test("android_step converts invalid requests into structured outcomes", async () => {
  const host = createCodexAndroidDynamicToolHost({
    runtime: createRuntime(),
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {},
  });

  assert.equal(response.success, false);
  assert.match(response.contentItems[0].text, /outcome: invalid_request/);
  assert.equal(response.metadata.android.outcome.status, "invalid_request");
  assert.equal(response.metadata.android.outcome.retryability, "none");
});

test("android_step reports unavailable atomic multi-touch as operator-required", async () => {
  const host = createCodexAndroidDynamicToolHost({
    runtime: createRuntime({ multiTouch: undefined }),
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      action: "multi_touch",
      pointers: [
        { x1: 400, y1: 800, x2: 300, y2: 800 },
        { x1: 600, y1: 800, x2: 700, y2: 800 },
      ],
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, false);
  assert.match(response.contentItems[0].text, /outcome: unsupported_capability/);
  assert.equal(response.metadata.android.outcome.status, "unsupported_capability");
  assert.equal(response.metadata.android.outcome.retryability, "operator_required");
});

test("android_step rejects malformed multi-touch before runtime dispatch", async () => {
  const runtime = createRuntime();
  const host = createCodexAndroidDynamicToolHost({
    runtime,
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      action: "multi_touch",
      pointers: [
        { x1: -1, y1: 800, x2: 300, y2: 800 },
        { x1: 600, y1: 800, x2: 700, y2: 800 },
      ],
      duration_ms: 49,
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, false);
  assert.match(response.contentItems[0].text, /outcome: invalid_request/);
  assert.equal(response.metadata.android.outcome.status, "invalid_request");
  assert.equal(runtime.calls.some((call) => call.fn === "multiTouch"), false);
});

test("transport loss becomes a structured retryable provider-unavailable outcome", async () => {
  const host = createCodexAndroidDynamicToolHost({
    runtime: createRuntime({
      async listDevices() {
        throw new Error(
          'initialize failed with 530: {"title":"Error 1033: Cloudflare Tunnel error","error_name":"tunnel_error","retryable":true,"retry_after":120}',
        );
      },
    }),
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const response = await host.executeToolCall({
    tool: "android_step",
    arguments: {
      action: "tap",
      x: 10,
      y: 20,
      post_observe_scope: "screen",
    },
  });

  assert.equal(response.success, false);
  assert.match(response.contentItems[0].text, /outcome: provider_unavailable/);
  assert.match(response.contentItems[0].text, /retryability: retry_same_request/);
  assert.equal(response.metadata.android.outcome.status, "provider_unavailable");
  assert.equal(response.metadata.android.outcome.retryability, "retry_same_request");
});
