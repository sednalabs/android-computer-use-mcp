import assert from "node:assert/strict";
import test from "node:test";

import {
  isTransientHierarchyTimeout,
  isTransientScreenshotTimeout,
  withTransientObservationRetry,
} from "./android_image_content_smoke.mjs";

const rawHierarchyTimeout =
  'tools/call failed: {"code":-32603,"message":"adb -s emulator-5554 exec-out uiautomator dump /dev/tty timed out after 20000 ms"}';
const rawScreenshotTimeout =
  'tools/call failed: {"code":-32603,"message":"adb -s emulator-5554 exec-out screencap -p timed out after 20000 ms"}';

test("recognizes transient hierarchy and screenshot timeout messages", () => {
  assert.equal(isTransientHierarchyTimeout(new Error(rawHierarchyTimeout)), true);
  assert.equal(isTransientHierarchyTimeout(new Error("UI hierarchy dump timed out")), true);
  assert.equal(isTransientScreenshotTimeout(new Error(rawScreenshotTimeout)), true);
  assert.equal(isTransientHierarchyTimeout(new Error("unrelated request timed out after 20000 ms")), false);
  assert.equal(isTransientScreenshotTimeout(new Error("unrelated request timed out after 20000 ms")), false);
});

test("retries transient observation timeouts", async () => {
  let attempts = 0;
  const result = await withTransientObservationRetry(
    "android.capture_screenshot",
    async () => {
      attempts += 1;
      if (attempts === 1) {
        throw new Error(rawScreenshotTimeout);
      }
      return "ready";
    },
    { retryDelayMs: 0 },
  );

  assert.equal(result, "ready");
  assert.equal(attempts, 2);
});

test("does not retry an unrelated failure", async () => {
  let attempts = 0;
  await assert.rejects(
    withTransientObservationRetry(
      "android.inspect_ui",
      async () => {
        attempts += 1;
        throw new Error("MCP session closed");
      },
      { retryDelayMs: 0 },
    ),
    /MCP session closed/,
  );
  assert.equal(attempts, 1);
});
