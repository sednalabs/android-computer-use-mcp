import assert from "node:assert/strict";
import test from "node:test";

import {
  isTransientHierarchyTimeout,
  withTransientHierarchyRetry,
} from "./android_image_content_smoke.mjs";

const rawAdbTimeout =
  'tools/call failed: {"code":-32603,"message":"adb -s emulator-5554 exec-out uiautomator dump /dev/tty timed out after 20000 ms"}';

test("recognizes both hierarchy timeout messages emitted by the MCP server", () => {
  assert.equal(isTransientHierarchyTimeout(new Error(rawAdbTimeout)), true);
  assert.equal(isTransientHierarchyTimeout(new Error("UI hierarchy dump timed out")), true);
  assert.equal(isTransientHierarchyTimeout(new Error("unrelated request timed out after 20000 ms")), false);
});

test("retries a transient raw uiautomator timeout", async () => {
  let attempts = 0;
  const result = await withTransientHierarchyRetry(
    "android.inspect_ui",
    async () => {
      attempts += 1;
      if (attempts === 1) {
        throw new Error(rawAdbTimeout);
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
    withTransientHierarchyRetry(
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
