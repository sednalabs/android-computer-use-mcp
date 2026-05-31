import crypto from "node:crypto";

import { createSolarLabReviewSession } from "./solarlab_review_session.js";

function isoNow(now) {
  return typeof now === "function" ? now() : new Date().toISOString();
}

function clone(value) {
  return value == null ? value : JSON.parse(JSON.stringify(value));
}

function summarizeScenario(review) {
  const scenario = review?.scripted?.scenario ?? null;
  const artifacts = Array.isArray(review?.scripted?.artifacts)
    ? review.scripted.artifacts.length
    : 0;
  return {
    scenario,
    artifacts,
    bundleDir: review?.scripted?.bundle_dir ?? null,
    manifestPath: review?.scripted?.manifest_path ?? null,
  };
}

export function createAndroidExecutionRun(options = {}) {
  const {
    createReviewSessionImpl = createSolarLabReviewSession,
    checkpoint = null,
    runId = checkpoint?.runId ?? crypto.randomUUID(),
    now = () => new Date().toISOString(),
    ...reviewOptions
  } = options;

  const reviewSession = createReviewSessionImpl(reviewOptions);
  const initialTimestamp = checkpoint?.createdAt ?? isoNow(now);
  const state = checkpoint
    ? {
        ...clone(checkpoint),
        updatedAt: isoNow(now),
      }
    : {
        runId,
        createdAt: initialTimestamp,
        updatedAt: initialTimestamp,
        status: "ready",
        serial: null,
        mode: "semantic-review",
        steps: [],
        checkpoints: [],
        lastResult: null,
      };

  async function recordStep(kind, payload = {}) {
    const step = {
      index: state.steps.length + 1,
      kind,
      at: isoNow(now),
      payload: clone(payload),
    };
    state.steps.push(step);
    state.updatedAt = step.at;
    return step;
  }

  function addCheckpoint(kind, payload = {}) {
    const checkpointEntry = {
      index: state.checkpoints.length + 1,
      kind,
      at: isoNow(now),
      payload: clone(payload),
    };
    state.checkpoints.push(checkpointEntry);
    state.updatedAt = checkpointEntry.at;
    return checkpointEntry;
  }

  return {
    ...reviewSession,

    getRunState() {
      return clone(state);
    },

    exportCheckpoint() {
      return clone(state);
    },

    async attachSerial(serial) {
      if (typeof reviewSession.setSerial === "function") {
        await reviewSession.setSerial(serial);
      }
      state.serial = serial;
      state.status = "attached";
      await recordStep("attach_serial", { serial });
      addCheckpoint("attached", { serial });
      return clone(state);
    },

    async captureBaseline({
      label = "android-run-baseline",
      includeResponseItems = true,
    } = {}) {
      const context = await reviewSession.ensureVisualContext(label);
      const responseItems =
        includeResponseItems && typeof reviewSession.visualContextToInputItems === "function"
          ? await reviewSession.visualContextToInputItems(context, {
              caption: `Android run ${state.runId} baseline`,
            })
          : null;

      const baseline = { context, responseItems };
      state.status = "baseline_captured";
      state.lastResult = { kind: "baseline", baseline: clone(baseline) };
      await recordStep("capture_baseline", {
        label,
        screenshotPath: context?.screenshotPath ?? null,
        uiDumpPath: context?.uiDumpPath ?? null,
      });
      addCheckpoint("baseline", baseline);
      return baseline;
    },

    async runFocusEarthReview({
      packageName = "com.sednalabs.solarlab",
      activity,
      freeformAction,
      includeResponseItems = true,
    } = {}) {
      const review = await reviewSession.reviewFocusEarth({
        packageName,
        activity,
        freeformAction,
        includeResponseItems,
      });
      state.status = "scenario_complete";
      state.lastResult = { kind: "focus_earth_review", review: clone(review) };
      await recordStep("focus_earth_review", summarizeScenario(review));
      addCheckpoint("focus_earth_review", {
        scriptedContext: review.scriptedContext,
        responseItems: review.responseItems ?? null,
      });
      return review;
    },

    async runImmersiveReview({
      packageName = "com.sednalabs.solarlab",
      activity,
      freeformAction,
      includeResponseItems = true,
    } = {}) {
      const review = await reviewSession.reviewImmersiveRoundtrip({
        packageName,
        activity,
        freeformAction,
        includeResponseItems,
      });
      state.status = "scenario_complete";
      state.lastResult = { kind: "immersive_review", review: clone(review) };
      await recordStep("immersive_review", summarizeScenario(review));
      addCheckpoint("immersive_review", {
        scriptedContext: review.scriptedContext,
        responseItems: review.responseItems ?? null,
      });
      return review;
    },
  };
}
