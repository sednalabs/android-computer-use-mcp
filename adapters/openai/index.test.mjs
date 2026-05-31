import test from "node:test";
import assert from "node:assert/strict";

import {
  createAndroidResponsesItemAdapter,
  createComputerCallOutputItem,
  createFunctionCallOutputItem,
  createMcpStreamableHttpClient,
  createOpenAiFileBroker,
  createOpenAiResponsesLoopDriver,
} from "./index.js";

test("package entrypoint re-exports native function_call_output wrapper", () => {
  const item = createFunctionCallOutputItem("call_123", [
    { type: "input_text", text: "hello" },
  ]);

  assert.deepEqual(item, {
    type: "function_call_output",
    call_id: "call_123",
    output: [{ type: "input_text", text: "hello" }],
  });
});

test("package entrypoint re-exports native computer_call_output wrapper", () => {
  const item = createComputerCallOutputItem(
    "call_456",
    "data:image/png;base64,Zm9v",
  );

  assert.deepEqual(item, {
    type: "computer_call_output",
    call_id: "call_456",
    output: {
      type: "computer_screenshot",
      image_url: "data:image/png;base64,Zm9v",
      detail: "original",
    },
  });
});

test("package entrypoint can build native function_call_output items from visual context", async () => {
  const adapter = createAndroidResponsesItemAdapter({
    readFile: async () => Buffer.from("png-bytes"),
  });

  const item = await adapter.functionCallOutputFromVisualContext("call_789", {
    serial: "emulator-5554",
    screenshotPath: "/tmp/current-screen.png",
  });

  assert.equal(item.type, "function_call_output");
  assert.equal(item.call_id, "call_789");
  assert.equal(item.output[0].type, "input_text");
  assert.equal(item.output[1].type, "input_image");
  assert.equal(item.output[1].detail, "original");
});

test("package entrypoint re-exports live-loop helpers", () => {
  assert.equal(typeof createMcpStreamableHttpClient, "function");
  assert.equal(typeof createOpenAiFileBroker, "function");
  assert.equal(typeof createOpenAiResponsesLoopDriver, "function");
});
