import test from "node:test";
import assert from "node:assert/strict";

import { createOpenAiFileBroker } from "./openai_file_broker.js";

test("OpenAI file broker uploads artifacts and returns file ids", async () => {
  let capturedBody = null;

  const broker = createOpenAiFileBroker({
    apiKey: "sk-test",
    readFile: async () => Buffer.from("png-bytes"),
    fetchImpl: async (url, init) => {
      assert.equal(url, "https://api.openai.com/v1/files");
      assert.equal(init.method, "POST");
      assert.equal(init.headers.authorization, "Bearer sk-test");
      capturedBody = init.body;
      return new Response(JSON.stringify({ id: "file_123" }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    },
  });

  const uploaded = await broker.uploadFile("/tmp/current-screen.png", {
    filename: "current-screen.png",
    mimeType: "image/png",
  });

  assert.deepEqual(uploaded, { file_id: "file_123" });
  assert.equal(capturedBody.get("purpose"), "user_data");
  assert.equal(capturedBody.get("file").name, "current-screen.png");
  assert.equal(capturedBody.get("file").type, "image/png");
});
