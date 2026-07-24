import test from "node:test";
import assert from "node:assert/strict";

import {
  contextFromObservation,
  createCodexThreadItemsAdapter,
  createCodexAndroidProviderManifest,
  createMessageItem,
  createThreadInjectItemsParams,
  ANDROID_PROVIDER_MANIFEST_DEFAULT_CAPABILITIES,
  ANDROID_PROVIDER_MANIFEST_OUTCOME_TAXONOMY,
  validateCodexAndroidProviderManifest,
} from "./index.js";

test("package entrypoint re-exports Codex bridge helpers", async () => {
  assert.equal(typeof createCodexThreadItemsAdapter, "function");
  assert.equal(typeof createMessageItem, "function");
  assert.equal(typeof createThreadInjectItemsParams, "function");
  assert.equal(typeof contextFromObservation, "function");
  assert.equal(typeof createCodexAndroidProviderManifest, "function");
  assert.equal(typeof validateCodexAndroidProviderManifest, "function");
  assert.deepEqual(Object.keys(ANDROID_PROVIDER_MANIFEST_DEFAULT_CAPABILITIES), [
    "appControl",
    "posture",
    "rawInput",
  ]);
  assert.deepEqual(ANDROID_PROVIDER_MANIFEST_OUTCOME_TAXONOMY.retryability, [
    "none",
    "observe_then_retry",
    "retry_same_request",
    "operator_required",
  ]);
});
