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
