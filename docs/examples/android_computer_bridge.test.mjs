import test from "node:test";
import assert from "node:assert/strict";

import { createAndroidComputerBridge } from "./android_computer_bridge.js";

function createRuntimeStub() {
  const calls = [];
  return {
    calls,
    async tap(x, y, options = {}) {
      calls.push({ fn: "tap", x, y, options });
      return { ok: true };
    },
    async doubleTap(x, y, options = {}) {
      calls.push({ fn: "doubleTap", x, y, options });
      return { ok: true };
    },
    async longPress(x, y, options = {}) {
      calls.push({ fn: "longPress", x, y, options });
      return { ok: true };
    },
    async swipe(x1, y1, x2, y2, options = {}) {
      calls.push({ fn: "swipe", x1, y1, x2, y2, options });
      return { ok: true };
    },
    async keyevent(keycode, options = {}) {
      calls.push({ fn: "keyevent", keycode, options });
      return { ok: true };
    },
    async keyCombination(keycodes, options = {}) {
      calls.push({ fn: "keyCombination", keycodes, options });
      return { ok: true };
    },
    async typeText(text, options = {}) {
      calls.push({ fn: "typeText", text, options });
      return { ok: true };
    },
    async captureState(label, options = {}) {
      calls.push({ fn: "captureState", label, options });
      return {
        serial: "emulator-5554",
        screenshotPath: `/tmp/${label}.png`,
        uiDumpPath: options.includeUi ? `/tmp/${label}.xml` : null,
      };
    },
    async displayScreenshot(path) {
      calls.push({ fn: "displayScreenshot", path });
      return path;
    },
  };
}

test("remapPoint maps frame coordinates through the zoomed region", () => {
  const runtime = createRuntimeStub();
  const bridge = createAndroidComputerBridge({
    runtime,
    deviceWidth: 1000,
    deviceHeight: 2000,
    frameWidth: 500,
    frameHeight: 1000,
  });

  bridge.setZoomRegion({ left: 100, top: 200, width: 400, height: 800 });
  const mapped = bridge.remapPoint(250, 500);

  assert.deepEqual(mapped, { x: 300, y: 600 });
});

test("executeActionBatch maps click coordinates through the current zoomed region", async () => {
  const runtime = createRuntimeStub();
  const bridge = createAndroidComputerBridge({
    runtime,
    deviceWidth: 1000,
    deviceHeight: 2000,
    frameWidth: 500,
    frameHeight: 1000,
  });

  bridge.setZoomRegion({ left: 100, top: 200, width: 400, height: 800 });
  await bridge.executeActionBatch([{ type: "click", x: 250, y: 500 }]);

  assert.deepEqual(runtime.calls[0], {
    fn: "tap",
    x: 300,
    y: 600,
    options: {},
  });
});

test("executeActionBatch uses runtime doubleTap for double_click gestures", async () => {
  const runtime = createRuntimeStub();
  const bridge = createAndroidComputerBridge({
    runtime,
    deviceWidth: 1000,
    deviceHeight: 2000,
    frameWidth: 500,
    frameHeight: 1000,
  });

  bridge.setZoomRegion({ left: 100, top: 200, width: 400, height: 800 });
  await bridge.executeActionBatch([{ type: "double_click", x: 250, y: 500 }]);

  assert.deepEqual(runtime.calls[0], {
    fn: "doubleTap",
    x: 300,
    y: 600,
    options: {},
  });
});

test("executeActionBatch uses runtime longPress for long_press gestures", async () => {
  const runtime = createRuntimeStub();
  const bridge = createAndroidComputerBridge({
    runtime,
    deviceWidth: 1000,
    deviceHeight: 2000,
    frameWidth: 500,
    frameHeight: 1000,
  });

  bridge.setZoomRegion({ left: 100, top: 200, width: 400, height: 800 });
  await bridge.executeActionBatch([
    { type: "long_press", x: 250, y: 500, duration_ms: 900, timeout_secs: 6 },
  ]);

  assert.deepEqual(runtime.calls[0], {
    fn: "longPress",
    x: 300,
    y: 600,
    options: { durationMs: 900, waitForSelector: null, timeoutSecs: 6 },
  });
});

test("executeActionBatch uses runtime keyCombination for multi-key keypress actions", async () => {
  const runtime = createRuntimeStub();
  const bridge = createAndroidComputerBridge({
    runtime,
    deviceWidth: 1000,
    deviceHeight: 2000,
  });

  await bridge.executeActionBatch([
    {
      type: "keypress",
      keys: ["Ctrl", "c"],
      keyMap: { Ctrl: "CTRL_LEFT", c: "C" },
      keyOptions: { timeoutSecs: 7 },
    },
  ]);

  assert.deepEqual(runtime.calls[0], {
    fn: "keyCombination",
    keycodes: ["CTRL_LEFT", "C"],
    options: { timeoutSecs: 7 },
  });
});

test("executeActionBatch supports batched zoom, click, and reset_zoom actions", async () => {
  const runtime = createRuntimeStub();
  const bridge = createAndroidComputerBridge({
    runtime,
    deviceWidth: 1080,
    deviceHeight: 1920,
    frameWidth: 540,
    frameHeight: 960,
  });

  const result = await bridge.executeActionBatch([
    {
      type: "zoom",
      region: { left: 100, top: 200, width: 400, height: 600 },
    },
    { type: "click", x: 270, y: 480 },
    { type: "reset_zoom" },
  ]);

  assert.equal(result.actionsExecuted, 3);
  assert.equal(runtime.calls[0].fn, "tap");
  assert.deepEqual(result.view.region, { left: 0, top: 0, width: 1080, height: 1920 });
});

test("scroll actions are bridged to raw swipe calls with expected scroll change", async () => {
  const runtime = createRuntimeStub();
  const bridge = createAndroidComputerBridge({
    runtime,
    deviceWidth: 1080,
    deviceHeight: 1920,
    frameWidth: 540,
    frameHeight: 960,
  });

  await bridge.executeActionBatch([
    { type: "scroll", x: 270, y: 480, scroll_y: 600, timeout_secs: 9 },
  ]);

  assert.equal(runtime.calls[0].fn, "swipe");
  assert.equal(runtime.calls[0].options.expectScrollChange, true);
  assert.equal(runtime.calls[0].options.timeoutSecs, 9);
});

test("captureView preserves base screenshot path and applies optional crop hook", async () => {
  const runtime = createRuntimeStub();
  const bridge = createAndroidComputerBridge({
    runtime,
    deviceWidth: 1080,
    deviceHeight: 1920,
    cropScreenshot: async (path, view) => `${path}#crop-${view.region.left}-${view.region.top}`,
  });

  bridge.setZoomRegion({ left: 50, top: 80, width: 400, height: 500 });
  const captured = await bridge.captureView("zoomed-state", { includeUi: true, display: true });

  assert.equal(captured.baseScreenshotPath, "/tmp/zoomed-state.png");
  assert.equal(captured.screenshotPath, "/tmp/zoomed-state.png#crop-50-80");
  assert.equal(runtime.calls.at(-1).fn, "displayScreenshot");
});
