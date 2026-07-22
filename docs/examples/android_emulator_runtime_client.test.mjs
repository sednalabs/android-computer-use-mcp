import test from "node:test";
import assert from "node:assert/strict";

import {
  ANDROID_TOOL_LOADING_PLAN,
  createAndroidEmulatorRuntime,
} from "./android_emulator_runtime_client.js";

test("waitForUiElement throws when MCP reports ok=false", async () => {
  const runtime = createAndroidEmulatorRuntime({
    callMcp: async (toolName) => {
      if (toolName === "android.wait_for_ui_element") {
        return {
          ok: false,
          satisfied: false,
          timed_out: true,
          artifacts: { hierarchy_path: "/tmp/wait-timeout.xml" },
        };
      }
      throw new Error(`unexpected tool call: ${toolName}`);
    },
  });

  await assert.rejects(
    runtime.waitForUiElement({ text: "Submit" }, { timeoutSecs: 1 }),
    (error) =>
      error instanceof Error &&
      error.message.includes("waitForUiElement did not satisfy selector") &&
      error.result?.ok === false &&
      error.result?.timed_out === true,
  );
});

test("findUiElement throws when selector resolution is ambiguous", async () => {
  const runtime = createAndroidEmulatorRuntime({
    callMcp: async (toolName) => {
      if (toolName === "android.find_ui_element") {
        return {
          ok: false,
          selection: {
            reason: "ambiguous_match",
            match_count: 2,
          },
          artifacts: { hierarchy_path: "/tmp/find-ui.xml" },
        };
      }
      throw new Error(`unexpected tool call: ${toolName}`);
    },
  });

  await assert.rejects(
    runtime.findUiElement({ text: "Search" }),
    (error) =>
      error instanceof Error &&
      error.message.includes("findUiElement did not resolve a unique selector") &&
      error.result?.selection?.reason === "ambiguous_match",
  );
});

test("scrollUntilVisible throws when swipe budget is exhausted", async () => {
  const runtime = createAndroidEmulatorRuntime({
    callMcp: async (toolName) => {
      if (toolName === "android.scroll_until_visible") {
        return {
          ok: false,
          matched: false,
          exhausted_swipe_budget: true,
          artifacts: { hierarchy_path: "/tmp/scroll-final.xml" },
        };
      }
      throw new Error(`unexpected tool call: ${toolName}`);
    },
  });

  await assert.rejects(
    runtime.scrollUntilVisible({ text: "Earth" }, { maxSwipes: 2 }),
    (error) =>
      error instanceof Error &&
      error.message.includes("scrollUntilVisible did not resolve a unique visible target") &&
      error.result?.ok === false &&
      error.result?.matched === false,
  );
});

test("launchApp throws when postcondition is not satisfied", async () => {
  const runtime = createAndroidEmulatorRuntime({
    callMcp: async (toolName) => {
      if (toolName === "android.launch_app") {
        return {
          ok: false,
          postcondition: {
            requested: true,
            satisfied: false,
            timed_out: true,
          },
        };
      }
      throw new Error(`unexpected tool call: ${toolName}`);
    },
  });

  await assert.rejects(
    runtime.launchApp("com.example.app", { waitForActivity: ".MainActivity" }),
    (error) =>
      error instanceof Error &&
      error.message.includes("launchApp did not satisfy the requested postcondition") &&
      error.result?.postcondition?.timed_out === true,
  );
});

test("runtime exposes harvested app control and posture helpers", async () => {
  const calls = [];
  const runtime = createAndroidEmulatorRuntime({
    callMcp: async (toolName, args) => {
      calls.push({ toolName, args });
      return { ok: true, postcondition: { satisfied: true } };
    },
  });
  await runtime.setSerial("emulator-5554");

  await runtime.listApps({ launcherOnly: false });
  await runtime.openUrl("https://example.test", { waitForPackage: "com.android.chrome" });
  await runtime.setOrientation("landscape", { timeoutSecs: 8 });
  await runtime.terminateApp("com.example.app");

  assert.deepEqual(
    calls.map((call) => call.toolName),
    [
      "android.list_apps",
      "android.open_url",
      "android.set_orientation",
      "android.terminate_app",
    ],
  );
  assert.equal(calls[0].args.launcher_only, false);
  assert.equal(calls[1].args.wait_for_package, "com.android.chrome");
  assert.equal(calls[2].args.orientation, "landscape");
  assert.equal(calls[2].args.timeout_secs, 8);
  assert.equal(calls[3].args.package_name, "com.example.app");
});

test("runtime exposes raw longPress helper", async () => {
  let longPressCall = null;
  const runtime = createAndroidEmulatorRuntime({
    callMcp: async (toolName, args) => {
      if (toolName === "android.input.long_press") {
        longPressCall = { toolName, args };
        return { ok: true, postcondition: { satisfied: true } };
      }
      throw new Error(`unexpected tool call: ${toolName}`);
    },
  });
  await runtime.setSerial("emulator-5554");

  await runtime.longPress(120, 240, { durationMs: 900, waitForSelector: { text: "Menu" } });

  assert.equal(longPressCall.toolName, "android.input.long_press");
  assert.equal(longPressCall.args.x, 120);
  assert.equal(longPressCall.args.y, 240);
  assert.equal(longPressCall.args.duration_ms, 900);
  assert.deepEqual(longPressCall.args.wait_for_selector, { text: "Menu" });
});

test("runtime exposes atomic multiTouch helper", async () => {
  let multiTouchCall = null;
  const runtime = createAndroidEmulatorRuntime({
    callMcp: async (toolName, args) => {
      if (toolName === "android.input.multi_touch") {
        multiTouchCall = { toolName, args };
        return { ok: true, capability: { status: "supported" } };
      }
      throw new Error(`unexpected tool call: ${toolName}`);
    },
  });
  await runtime.setSerial("emulator-5554");
  const pointers = [
    { x1: 400, y1: 800, x2: 300, y2: 800 },
    { x1: 600, y1: 800, x2: 700, y2: 800 },
  ];

  await runtime.multiTouch(pointers, { durationMs: 420, timeoutSecs: 7 });

  assert.equal(multiTouchCall.toolName, "android.input.multi_touch");
  assert.equal(multiTouchCall.args.serial, "emulator-5554");
  assert.deepEqual(multiTouchCall.args.pointers, pointers);
  assert.equal(multiTouchCall.args.duration_ms, 420);
  assert.equal(multiTouchCall.args.timeout_secs, 7);
});

test("typeIntoElement forwards timeout and stores stabilized hierarchy artifact", async () => {
  let typeIntoCall = null;
  const runtime = createAndroidEmulatorRuntime({
    callMcp: async (toolName, args) => {
      if (toolName === "android.type_into_element") {
        typeIntoCall = { toolName, args };
        return {
          ok: true,
          typed: true,
          artifacts: {
            hierarchy_path: "/tmp/type-pre.xml",
            stable_hierarchy_path: "/tmp/type-stable.xml",
          },
        };
      }
      throw new Error(`unexpected tool call: ${toolName}`);
    },
  });

  const result = await runtime.typeIntoElement(
    { resource_id: "com.example:id/query" },
    "earth",
    { timeoutSecs: 9, hierarchyFilename: "custom-type.xml" },
  );

  assert.equal(result.ok, true);
  assert.equal(typeIntoCall?.args?.timeout_secs, 9);
  assert.equal(typeIntoCall?.args?.hierarchy_filename, "custom-type.xml");
  assert.equal(runtime.getState().lastUiDumpPath, "/tmp/type-stable.xml");
});

test("semanticAction maps a focus target to the provider body_query field", async () => {
  let semanticCall = null;
  const runtime = createAndroidEmulatorRuntime({
    callMcp: async (toolName, args) => {
      if (toolName === "solarlab.semantic_action") {
        semanticCall = { toolName, args };
        return { ok: true, acknowledgment: { acknowledged: true } };
      }
      throw new Error(`unexpected tool call: ${toolName}`);
    },
  });
  await runtime.setSerial("emulator-5554");

  await runtime.semanticAction("focus_body", { target: "comet", timeout_secs: 9 });

  assert.equal(semanticCall.toolName, "solarlab.semantic_action");
  assert.deepEqual(semanticCall.args, {
    serial: "emulator-5554",
    action: "focus_body",
    body_query: "comet",
    timeout_secs: 9,
  });
});

test("getToolLoadingPlan exposes eager bootstrap tools and deferred semantic tools", () => {
  const runtime = createAndroidEmulatorRuntime({
    callMcp: async () => ({ ok: true }),
  });

  const plan = runtime.getToolLoadingPlan();
  assert.equal(plan.bootstrap.loading, "eager");
  assert.equal(plan.semanticUi.loading, "deferred");
  assert.deepEqual(plan.bootstrap.tools, [...ANDROID_TOOL_LOADING_PLAN.bootstrap.tools]);
  assert.deepEqual(plan.solarlab.tools, [...ANDROID_TOOL_LOADING_PLAN.solarlab.tools]);
});

test("installBuildFromRun preserves the native execution contract at the MCP boundary", async () => {
  const calls = [];
  const runtime = createAndroidEmulatorRuntime({
    defaultSerial: "emulator-5554",
    callMcp: async (toolName, args) => {
      calls.push({ toolName, args });
      return { ok: true, serial: "emulator-5554" };
    },
  });

  await runtime.installBuildFromRun({
    workflow_run_id: 42,
    artifact_name: "android-build",
    contract_version: "android-provider-execution/v1",
    target: {
      environment_id: "environment-1",
      provider_instance_id: "provider-1",
      session_id: "session-1",
      device_serial: "emulator-5554",
      expected_build: {
        repository: "example/android-app",
        commit_sha: "abcdef0123456789",
        workflow_run_id: 42,
        artifact_name: "android-build",
        artifact_sha256: "sha256:artifact",
      },
    },
    install: { launch_after_install: false },
  });

  assert.deepEqual(calls, [{
    toolName: "interactive_session.install_build_from_run",
    args: {
      workflow_run_id: 42,
      artifact_name: "android-build",
      repository: null,
      serial: "emulator-5554",
      timeout_secs: null,
      contract_version: "android-provider-execution/v1",
      target: {
        environment_id: "environment-1",
        provider_instance_id: "provider-1",
        session_id: "session-1",
        device_serial: "emulator-5554",
        expected_build: {
          repository: "example/android-app",
          commit_sha: "abcdef0123456789",
          workflow_run_id: 42,
          artifact_name: "android-build",
          artifact_sha256: "sha256:artifact",
        },
      },
      install: { launch_after_install: false },
    },
  }]);
});

test("deferred semantic tool calls invoke ensureToolsAvailable before MCP dispatch", async () => {
  const calls = [];
  const ensures = [];
  const runtime = createAndroidEmulatorRuntime({
    ensureToolsAvailable: async (request) => {
      ensures.push(request);
    },
    callMcp: async (toolName, args) => {
      calls.push({ toolName, args });
      if (toolName === "android.tap_element") {
        return {
          ok: true,
          artifacts: { hierarchy_path: "/tmp/post-tap.xml" },
        };
      }
      throw new Error(`unexpected tool call: ${toolName}`);
    },
  });

  await runtime.tapElement({ text: "Search" }, { timeoutSecs: 4 });

  assert.equal(ensures.length, 1);
  assert.equal(ensures[0].group, "semanticUi");
  assert.equal(ensures[0].loading, "deferred");
  assert.equal(ensures[0].query, ANDROID_TOOL_LOADING_PLAN.semanticUi.query);
  assert.deepEqual(ensures[0].tools, [...ANDROID_TOOL_LOADING_PLAN.semanticUi.tools]);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].toolName, "android.tap_element");
});

test("eager bootstrap tool calls do not invoke deferred loading hooks", async () => {
  const ensures = [];
  const runtime = createAndroidEmulatorRuntime({
    ensureToolsAvailable: async (request) => {
      ensures.push(request);
    },
    callMcp: async (toolName) => {
      if (toolName === "android.health") {
        return { ok: true };
      }
      throw new Error(`unexpected tool call: ${toolName}`);
    },
  });

  const result = await runtime.health();

  assert.equal(result.ok, true);
  assert.equal(ensures.length, 0);
});

test("toolSearch fallback is used when no explicit ensureToolsAvailable hook is provided", async () => {
  const searches = [];
  const runtime = createAndroidEmulatorRuntime({
    toolSearch: async (request) => {
      searches.push(request);
      return [{ name: "solarlab.scenario.stage_first_focus_earth" }];
    },
    callMcp: async (toolName) => {
      if (toolName === "solarlab.scenario.stage_first_focus_earth") {
        return { ok: true, artifacts: [] };
      }
      throw new Error(`unexpected tool call: ${toolName}`);
    },
  });

  await runtime.runSolarLabScenario("stage_first_focus_earth");

  assert.equal(searches.length, 1);
  assert.equal(searches[0].group, "solarlab");
  assert.equal(searches[0].query, ANDROID_TOOL_LOADING_PLAN.solarlab.query);
  assert.deepEqual(searches[0].toolNames, [...ANDROID_TOOL_LOADING_PLAN.solarlab.tools]);
});
