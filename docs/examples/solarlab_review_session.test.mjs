import test from "node:test";
import assert from "node:assert/strict";

import { createSolarLabReviewSession } from "./solarlab_review_session.js";

function createHelpersStub() {
  const calls = [];
  const helpers = {
    calls,
    async ensureVisualContext(label) {
      calls.push({ fn: "ensureVisualContext", label });
      return {
        serial: "emulator-5554",
        screenshotPath: `/tmp/${label}.png`,
        uiDumpPath: `/tmp/${label}.xml`,
      };
    },
    async focusEarth({ packageName, activity }) {
      calls.push({ fn: "focusEarth", packageName, activity });
      return {
        scenario: "stage_first_focus_earth",
        manifest_path: "/tmp/focus-earth/manifest.json",
        bundle_dir: "/tmp/focus-earth",
        logcat_path: "/tmp/focus-earth/logcat.txt",
        artifacts: [
          {
            screenshot: "/tmp/focus-earth/final.png",
            ui_dump: "/tmp/focus-earth/final.xml",
          },
        ],
      };
    },
    async immersiveRoundtrip({ packageName, activity }) {
      calls.push({ fn: "immersiveRoundtrip", packageName, activity });
      return {
        scenario: "stage_first_immersive_roundtrip",
        artifacts: [],
        manifest_path: null,
        bundle_dir: null,
        logcat_path: null,
      };
    },
    async runExplorationStep(label, action) {
      calls.push({ fn: "runExplorationStep", label });
      await action({ kind: "runtime-stub" });
      return {
        before: {
          screenshotPath: `/tmp/${label}-before.png`,
          uiDumpPath: `/tmp/${label}-before.xml`,
        },
        after: {
          screenshotPath: `/tmp/${label}-after.png`,
          uiDumpPath: `/tmp/${label}-after.xml`,
        },
      };
    },
  };
  return helpers;
}

test("reviewFocusEarth can emit model-ready Responses items for starting, scripted, and freeform contexts", async () => {
  const helpers = createHelpersStub();
  const adapterCalls = [];
  const session = createSolarLabReviewSession({
    createSolarLabHelpersImpl: () => helpers,
    createResponsesItemAdapterImpl: () => ({
      async visualContextToInputItems(context, options) {
        adapterCalls.push({ fn: "visualContextToInputItems", context, options });
        return [{ type: "input_text", text: options.caption }];
      },
      async scenarioResultToInputItems(result, options) {
        adapterCalls.push({ fn: "scenarioResultToInputItems", result, options });
        return [{ type: "input_text", text: options.caption }];
      },
    }),
  });

  const review = await session.reviewFocusEarth({
    includeResponseItems: true,
    freeformAction: async () => {},
  });

  assert.equal(review.strategy, "scenario-first");
  assert.deepEqual(
    review.responseItems,
    {
      startingContext: [{ type: "input_text", text: "Focus Earth review starting context" }],
      scripted: [{ type: "input_text", text: "Focus Earth review scripted result" }],
      freeformAfter: [{ type: "input_text", text: "Focus Earth review freeform result" }],
    },
  );
  assert.deepEqual(
    adapterCalls.map((call) => call.fn),
    [
      "visualContextToInputItems",
      "scenarioResultToInputItems",
      "visualContextToInputItems",
    ],
  );
});

test("reviewImmersiveRoundtrip keeps responseItems null unless explicitly requested", async () => {
  const helpers = createHelpersStub();
  let adapterUsed = false;
  const session = createSolarLabReviewSession({
    createSolarLabHelpersImpl: () => helpers,
    createResponsesItemAdapterImpl: () => ({
      async visualContextToInputItems() {
        adapterUsed = true;
        return [];
      },
      async scenarioResultToInputItems() {
        adapterUsed = true;
        return [];
      },
    }),
  });

  const review = await session.reviewImmersiveRoundtrip();

  assert.equal(review.responseItems, null);
  assert.equal(adapterUsed, false);
});
