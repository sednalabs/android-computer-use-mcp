import test from "node:test";
import assert from "node:assert/strict";

import { createAndroidExecutionRun } from "./android_execution_run.js";

function createReviewSessionStub() {
  const calls = [];
  return {
    calls,
    async setSerial(serial) {
      calls.push({ fn: "setSerial", serial });
      return serial;
    },
    async ensureVisualContext(label) {
      calls.push({ fn: "ensureVisualContext", label });
      return {
        serial: "emulator-5554",
        screenshotPath: `/tmp/${label}.png`,
        uiDumpPath: `/tmp/${label}.xml`,
      };
    },
    async visualContextToInputItems(context, options) {
      calls.push({ fn: "visualContextToInputItems", context, options });
      return [{ type: "input_text", text: options.caption }];
    },
    async reviewFocusEarth(options) {
      calls.push({ fn: "reviewFocusEarth", options });
      return {
        scripted: {
          scenario: "stage_first_focus_earth",
          artifacts: [{ screenshot: "/tmp/final.png", ui_dump: "/tmp/final.xml" }],
          bundle_dir: "/tmp/focus-earth",
          manifest_path: "/tmp/focus-earth/manifest.json",
        },
        scriptedContext: {
          screenshotPath: "/tmp/final.png",
          uiDumpPath: "/tmp/final.xml",
        },
        responseItems: {
          scripted: [{ type: "input_text", text: "scripted result" }],
        },
      };
    },
    async reviewImmersiveRoundtrip(options) {
      calls.push({ fn: "reviewImmersiveRoundtrip", options });
      return {
        scripted: {
          scenario: "stage_first_immersive_roundtrip",
          artifacts: [],
          bundle_dir: null,
          manifest_path: null,
        },
        scriptedContext: {
          screenshotPath: "/tmp/immersive.png",
          uiDumpPath: "/tmp/immersive.xml",
        },
        responseItems: null,
      };
    },
  };
}

test("captureBaseline records durable run state and checkpoint data", async () => {
  const reviewSession = createReviewSessionStub();
  const run = createAndroidExecutionRun({
    createReviewSessionImpl: () => reviewSession,
    runId: "run-123",
    now: () => "2026-04-20T09:30:00.000Z",
  });

  await run.attachSerial("emulator-5554");
  const baseline = await run.captureBaseline();
  const state = run.getRunState();

  assert.equal(state.runId, "run-123");
  assert.equal(state.serial, "emulator-5554");
  assert.equal(state.status, "baseline_captured");
  assert.equal(state.steps.length, 2);
  assert.equal(state.checkpoints.length, 2);
  assert.equal(baseline.responseItems[0].text, "Android run run-123 baseline");
});

test("runFocusEarthReview stores the latest review result in a resumable checkpoint", async () => {
  const reviewSession = createReviewSessionStub();
  const run = createAndroidExecutionRun({
    createReviewSessionImpl: () => reviewSession,
    runId: "run-focus-earth",
    now: () => "2026-04-20T09:31:00.000Z",
  });

  const review = await run.runFocusEarthReview();
  const checkpoint = run.exportCheckpoint();

  assert.equal(review.scripted.scenario, "stage_first_focus_earth");
  assert.equal(checkpoint.status, "scenario_complete");
  assert.equal(checkpoint.lastResult.kind, "focus_earth_review");
  assert.equal(checkpoint.checkpoints.at(-1).kind, "focus_earth_review");
  assert.equal(
    checkpoint.checkpoints.at(-1).payload.responseItems.scripted[0].text,
    "scripted result",
  );
});

test("a run can resume from a prior checkpoint without losing history", () => {
  const resumed = createAndroidExecutionRun({
    createReviewSessionImpl: () => createReviewSessionStub(),
    checkpoint: {
      runId: "run-resume",
      createdAt: "2026-04-20T09:00:00.000Z",
      updatedAt: "2026-04-20T09:05:00.000Z",
      status: "scenario_complete",
      serial: "emulator-5554",
      mode: "semantic-review",
      steps: [{ index: 1, kind: "attach_serial", at: "2026-04-20T09:00:10.000Z", payload: { serial: "emulator-5554" } }],
      checkpoints: [{ index: 1, kind: "attached", at: "2026-04-20T09:00:10.000Z", payload: { serial: "emulator-5554" } }],
      lastResult: { kind: "baseline", baseline: { context: { screenshotPath: "/tmp/old.png" } } },
    },
    now: () => "2026-04-20T09:32:00.000Z",
  });

  const state = resumed.getRunState();

  assert.equal(state.runId, "run-resume");
  assert.equal(state.serial, "emulator-5554");
  assert.equal(state.steps.length, 1);
  assert.equal(state.checkpoints.length, 1);
  assert.equal(state.updatedAt, "2026-04-20T09:32:00.000Z");
});
