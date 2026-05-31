import test from "node:test";
import assert from "node:assert/strict";

import {
  contextFromObservation,
  createCodexThreadItemsAdapter,
  createThreadInjectItemsParams,
} from "./codex_thread_items.js";

test("visualContextMessage keeps Codex thread items to text plus image", async () => {
  const adapter = createCodexThreadItemsAdapter({
    readFile: async () => Buffer.from("png-bytes"),
  });

  const message = await adapter.visualContextMessage({
    serial: "emulator-5554",
    screenshotPath: "/tmp/current.png",
    uiDumpPath: "/tmp/current.xml",
  });

  assert.equal(message.type, "message");
  assert.equal(message.role, "assistant");
  assert.equal(message.content.length, 2);
  assert.equal(message.content[0].type, "input_text");
  assert.match(message.content[0].text, /current\.xml/);
  assert.deepEqual(message.content[1], {
    type: "input_image",
    image_url: "data:image/png;base64,cG5nLWJ5dGVz",
    detail: "original",
  });
});

test("threadInjectItemsFromVisualContext wraps a message for app-server injection", async () => {
  const adapter = createCodexThreadItemsAdapter({
    readFile: async () => Buffer.from("png-bytes"),
  });

  const payload = await adapter.threadInjectItemsFromVisualContext("thr_123", {
    serial: "emulator-5554",
    screenshotPath: "/tmp/current.png",
    uiDumpPath: "/tmp/current.xml",
  });

  assert.equal(payload.threadId, "thr_123");
  assert.equal(payload.items.length, 1);
  assert.equal(payload.items[0].type, "message");
});

test("contextFromObservation normalizes runtime observation results", async () => {
  assert.deepEqual(
    contextFromObservation({
      serial: "emulator-5554",
      artifacts: {
        screenshot_path: "/tmp/current.png",
        hierarchy_path: "/tmp/current.xml",
      },
    }),
    {
      serial: "emulator-5554",
      screenshotPath: "/tmp/current.png",
      uiDumpPath: "/tmp/current.xml",
      lastScenarioResult: null,
    },
  );
});

test("createThreadInjectItemsParams uses the app-server camelCase field name", async () => {
  assert.deepEqual(createThreadInjectItemsParams("thr_456", [{ type: "message" }]), {
    threadId: "thr_456",
    items: [{ type: "message" }],
  });
});
