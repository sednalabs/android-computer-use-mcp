import assert from "node:assert/strict";
import test from "node:test";
import { summarizeDisplayGeometry, summarizeInputGeometry, summarizeWmSize } from "./android_provider_geometry.mjs";

const viewport = "Viewport INTERNAL: displayId=0, uniqueId=private-display, port=0, orientation=1, logicalFrame=[0, 0, 2400, 1080], physicalFrame=[0, 0, 2400, 1080], deviceSize=[2400, 1080], isActive=[1]";
const mapper = `  Device 4: private-device-name
    Touch Input Mapper (mode - DIRECT):
      Parameters:
        GestureMode: MULTI_TOUCH
        DeviceType: TOUCH_SCREEN
        OrientationAware: true
        Orientation: ROTATION_0
      Raw Touch Axes:
        X: min=0, max=32767, flat=0, fuzz=0, resolution=0
        Y: min=0, max=32767, flat=0, fuzz=0, resolution=0
      Affine Transformation:
        X scale: 1.000
        X ymix: 0.000
        X offset: 0.000
        Y xmix: 0.000
        Y scale: 1.000
        Y offset: 0.000
      ${viewport}
    Keyboard Input Mapper:
      FocusedWindow: private-window-title
`;

test("input evidence retains active touch geometry without names or window data", () => {
  const summary = summarizeInputGeometry(`${mapper}\n${viewport}\nFocusedApplication: private-app`);
  assert.equal(summary.viewports.length, 1);
  assert.deepEqual(summary.viewports[0], {
    display_id: 0, orientation: 1, logical_frame: [0, 0, 2400, 1080],
    physical_frame: [0, 0, 2400, 1080], device_size: [2400, 1080], active: true, valid: undefined,
  });
  assert.equal(summary.touch_mappers[0].mode, "DIRECT");
  assert.equal(summary.touch_mappers[0].OrientationAware, "true");
  assert.equal(summary.touch_mappers[0].Orientation, "ROTATION_0");
  assert.deepEqual(summary.touch_mappers[0].raw_axes.X, { min: 0, max: 32767, flat: 0, fuzz: 0, resolution: 0 });
  assert.deepEqual(summary.touch_mappers[0].affine, {
    "X scale": 1, "X ymix": 0, "X offset": 0, "Y xmix": 0, "Y scale": 1, "Y offset": 0,
  });
  assert.doesNotMatch(JSON.stringify(summary), /private-|Focused|Keyboard/);
});

test("display and wm evidence retain logical frames and sizes without identifiers", () => {
  const summary = summarizeDisplayGeometry(`
  mViewports=[DisplayViewport{type=INTERNAL, valid=true, isActive=true, displayId=0, uniqueId='private-display', orientation=1, logicalFrame=Rect(0, 0 - 2400, 1080), physicalFrame=Rect(0, 0 - 2400, 1080), deviceWidth=2400, deviceHeight=1080}]
  mBaseDisplayInfo=DisplayInfo{"private-display-name", displayId 0, real 2400 x 1080, app 2400 x 996, rotation 1, state ON}
  mOverrideDisplayInfo=DisplayInfo{"private-display-name", displayId 0, real 2400 x 1080, app 2328 x 996, rotation 1, state ON}
  private-user-setting=private-value
  `);
  assert.deepEqual(summary.viewports[0].logical_frame, [0, 0, 2400, 1080]);
  assert.deepEqual(summary.viewports[0].device_size, [2400, 1080]);
  assert.equal(summary.viewports[0].active, true);
  assert.equal(summary.viewports[0].valid, true);
  assert.equal(summary.logical_displays.length, 2);
  assert.deepEqual(summary.logical_displays[1], {
    source: "mOverrideDisplayInfo", display_id: 0, real_size: [2400, 1080], app_size: [2328, 996], rotation: 1,
  });
  assert.doesNotMatch(JSON.stringify(summary), /private-/);
  assert.deepEqual(summarizeWmSize("Physical size: 1080x2400\nOverride size: 540x1200\nprivate-extra"), {
    physical: [1080, 2400], override: [540, 1200],
  });
});

test("geometry evidence bounds repeated mappers and ignores malformed scalar data", () => {
  const summary = summarizeInputGeometry(mapper.repeat(40));
  assert.equal(summary.touch_mappers.length, 8);
  assert.ok(JSON.stringify(summary).length < 16000);
  const malformed = summarizeInputGeometry(mapper.replace("X scale: 1.000", "X scale: private-value"));
  assert.equal(malformed.touch_mappers[0].affine["X scale"], undefined);
  assert.doesNotMatch(JSON.stringify(malformed), /private-/);
  assert.deepEqual(summarizeWmSize("Physical size: 1080x2400 private-value"), {});
});
