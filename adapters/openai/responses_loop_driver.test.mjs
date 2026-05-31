import test from "node:test";
import assert from "node:assert/strict";

import { createOpenAiResponsesLoopDriver } from "./responses_loop_driver.js";

test("Responses loop driver runs a native observe round and prefers file ids for hosted artifacts", async () => {
  const requests = [];

  const mcpClient = {
    async initialize() {},
    async close() {},
    async callTool(name) {
      if (name === "android.list_devices") {
        return {
          ok: true,
          devices: [{ serial: "emulator-5554" }],
        };
      }
      if (name === "android.wait_for_stable_ui") {
        return {
          ok: true,
          serial: "emulator-5554",
          artifacts: {
            screenshot_path: "/tmp/current.png",
            hierarchy_path: "/tmp/current.xml",
          },
        };
      }
      throw new Error(`Unexpected MCP tool call: ${name}`);
    },
  };

  const driver = createOpenAiResponsesLoopDriver({
    mcpClient,
    openAiApiKey: "sk-test",
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
    fetchImpl: async (url, init) => {
      requests.push({ url, init });

      if (url.endsWith("/files")) {
        const file = init.body.get("file");
        return new Response(
          JSON.stringify({ id: `file_for_${file.name.replace(/\W+/g, "_")}` }),
          {
            status: 200,
            headers: { "content-type": "application/json" },
          },
        );
      }

      if (!url.endsWith("/responses")) {
        throw new Error(`Unexpected fetch target: ${url}`);
      }

      const payload = JSON.parse(init.body);
      if (!payload.previous_response_id) {
        return new Response(
          JSON.stringify({
            id: "resp_1",
            output: [
              {
                type: "function_call",
                call_id: "call_observe",
                name: "android_observe",
                arguments: JSON.stringify({
                  include_ui: true,
                  stable: true,
                }),
              },
            ],
          }),
          {
            status: 200,
            headers: { "content-type": "application/json" },
          },
        );
      }

      const toolOutput = payload.input[0];
      assert.equal(toolOutput.type, "function_call_output");
      assert.equal(toolOutput.call_id, "call_observe");
      assert.equal(toolOutput.output[1].type, "input_image");
      assert.equal(toolOutput.output[1].file_id, "file_for_current_png");
      assert.equal(toolOutput.output[2].type, "input_file");
      assert.equal(toolOutput.output[2].file_id, "file_for_current_xml");

      return new Response(
        JSON.stringify({
          id: "resp_2",
          output_text: "The Android screen is captured and ready for the next step.",
          output: [],
        }),
        {
          status: 200,
          headers: { "content-type": "application/json" },
        },
      );
    },
  });

  const result = await driver.run({
    prompt: "Observe the current Android state.",
  });

  assert.equal(
    result.output_text,
    "The Android screen is captured and ready for the next step.",
  );
  assert.deepEqual(result.function_calls_handled, [
    {
      name: "android_observe",
      call_id: "call_observe",
    },
  ]);
  await driver.close();
});
