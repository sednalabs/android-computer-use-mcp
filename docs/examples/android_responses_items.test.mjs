import test from "node:test";
import assert from "node:assert/strict";

import { createAndroidResponsesItemAdapter } from "./android_responses_items.js";

test("inputImageFromPath falls back to a data URL when no external image broker is provided", async () => {
  const adapter = createAndroidResponsesItemAdapter({
    readFile: async () => Buffer.from("png-bytes"),
  });

  const item = await adapter.inputImageFromPath("/tmp/screen.png");

  assert.equal(item.type, "input_image");
  assert.equal(item.detail, "original");
  assert.match(item.image_url, /^data:image\/png;base64,/);
});

test("inputImageFromPath prefers uploaded file ids when configured for longer hosted loops", async () => {
  const adapter = createAndroidResponsesItemAdapter({
    preferFileIdForImages: true,
    readFile: async () => {
      throw new Error("readFile should not run when uploadFile resolves the image");
    },
    uploadFile: async (filePath, options) => ({
      file_id: `image-file:${filePath}:${options.filename}`,
    }),
  });

  const item = await adapter.inputImageFromPath("/tmp/screen.png");

  assert.deepEqual(item, {
    type: "input_image",
    file_id: "image-file:/tmp/screen.png:screen.png",
    detail: "original",
  });
});

test("inputFileFromPath prefers uploaded file ids when the host can broker them", async () => {
  const adapter = createAndroidResponsesItemAdapter({
    readFile: async () => {
      throw new Error("readFile should not run when uploadFile resolves the artifact");
    },
    uploadFile: async (filePath, options) => ({
      file_id: `file-for:${filePath}:${options.filename}`,
    }),
  });

  const item = await adapter.inputFileFromPath("/tmp/state.xml");

  assert.deepEqual(item, {
    type: "input_file",
    file_id: "file-for:/tmp/state.xml:state.xml",
  });
});

test("inputFileFromPath falls back to a hosted file URL when one is available", async () => {
  const adapter = createAndroidResponsesItemAdapter({
    readFile: async () => {
      throw new Error("readFile should not run when fileUrlForPath resolves the artifact");
    },
    fileUrlForPath: async (filePath) => `https://artifacts.example.test${filePath}`,
  });

  const item = await adapter.inputFileFromPath("/tmp/logcat.txt");

  assert.deepEqual(item, {
    type: "input_file",
    file_url: "https://artifacts.example.test/tmp/logcat.txt",
  });
});

test("visualContextToInputItems emits summary text plus screenshot and ui dump items", async () => {
  const adapter = createAndroidResponsesItemAdapter({
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const items = await adapter.visualContextToInputItems(
    {
      serial: "emulator-5554",
      screenshotPath: "/tmp/inspect-ui.png",
      uiDumpPath: "/tmp/inspect-ui.xml",
    },
    { caption: "Fresh Android state" },
  );

  assert.equal(items[0].type, "input_text");
  assert.match(items[0].text, /Fresh Android state/);
  assert.deepEqual(
    items.slice(1).map((item) => item.type),
    ["input_image", "input_file"],
  );
  assert.match(items[1].image_url, /^data:image\/png;base64,/);
  assert.equal(items[2].filename, "inspect-ui.xml");
  assert.match(items[2].file_data, /^data:application\/xml;base64,/);
});

test("functionCallOutputFromVisualContext packages model-native output items", async () => {
  const adapter = createAndroidResponsesItemAdapter({
    readFile: async (filePath) => Buffer.from(`bytes:${filePath}`),
  });

  const item = await adapter.functionCallOutputFromVisualContext("call_visual", {
    serial: "emulator-5554",
    screenshotPath: "/tmp/inspect-ui.png",
    uiDumpPath: "/tmp/inspect-ui.xml",
  });

  assert.equal(item.type, "function_call_output");
  assert.equal(item.call_id, "call_visual");
  assert.deepEqual(
    item.output.map((entry) => entry.type),
    ["input_text", "input_image", "input_file"],
  );
  assert.equal(item.output[1].detail, "original");
});

test("computerCallOutputFromPath packages a native computer screenshot output", async () => {
  const adapter = createAndroidResponsesItemAdapter({
    readFile: async () => Buffer.from("png-bytes"),
  });

  const item = await adapter.computerCallOutputFromPath("call_screen", "/tmp/screen.png");

  assert.deepEqual(item, {
    type: "computer_call_output",
    call_id: "call_screen",
    output: {
      type: "computer_screenshot",
      image_url: item.output.image_url,
      detail: "original",
    },
  });
  assert.match(item.output.image_url, /^data:image\/png;base64,/);
});

test("scenarioResultToInputItems maps final screenshot, ui dump, manifest, and logcat", async () => {
  const adapter = createAndroidResponsesItemAdapter({
    readFile: async (filePath) => Buffer.from(`artifact:${filePath}`),
  });

  const items = await adapter.scenarioResultToInputItems(
    {
      scenario: "stage_first_focus_earth",
      bundle_dir: "/tmp/focus-earth-bundle",
      manifest_path: "/tmp/focus-earth-bundle/manifest.json",
      logcat_path: "/tmp/focus-earth-bundle/logcat.txt",
      artifacts: [
        {
          screenshot: "/tmp/focus-earth-bundle/final.png",
          ui_dump: "/tmp/focus-earth-bundle/final.xml",
        },
      ],
    },
    { caption: "Scenario proof" },
  );

  assert.equal(items[0].type, "input_text");
  assert.match(items[0].text, /Scenario proof/);
  assert.match(items[1].image_url, /^data:image\/png;base64,/);
  assert.deepEqual(
    items.slice(2).map((item) => item.filename),
    ["final.xml", "manifest.json", "logcat.txt"],
  );
});
