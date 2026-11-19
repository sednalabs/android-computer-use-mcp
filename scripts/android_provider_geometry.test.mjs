import assert from "node:assert/strict";
import test from "node:test";
import { summarizeDisplayGeometry, summarizeInputGeometry, summarizeWmSize } from "./android_provider_geometry.mjs";
import { classifyAnrOwner, summarizeBuildIdentity, summarizeVerification } from "./android_provider_diagnostics.mjs";

const viewport = "Viewport INTERNAL: displayId=0, uniqueId=private-display, port=0, orientation=1, logicalFrame=[0, 0, 2400, 1080], physicalFrame=[0, 0, 2400, 1080], deviceSize=[2400, 1080], isActive=[1]";
const mapper = `  Device 4: private-device-name
    Touch Input Mapper (mode - DIRECT):
      Parameters:
        GestureMode: MULTI_TOUCH
        DeviceType: TOUCH_SCREEN
        OrientationAware: true
        Orientation: Rotation0
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
  assert.equal(summary.touch_mappers[0].Orientation, "Rotation0");
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

test("orientation evidence preserves an explicit enum token without assuming missing rotation", () => {
  for (const token of ["Rotation0", "Rotation90", "ROTATION_0", "0"]) {
    const summary = summarizeInputGeometry(mapper.replace("Orientation: Rotation0", `Orientation: ${token}`));
    assert.equal(summary.touch_mappers[0].Orientation, token);
    assert.equal(summary.touch_mappers[0].orientation_present, true);
  }
  const missing = summarizeInputGeometry(mapper.replace("        Orientation: Rotation0\n", ""));
  assert.equal(missing.touch_mappers[0].Orientation, undefined);
  assert.equal(missing.touch_mappers[0].orientation_present, undefined);
  const unsafe = summarizeInputGeometry(mapper.replace("Orientation: Rotation0", "Orientation: 0 /private/path"));
  assert.equal(unsafe.touch_mappers[0].Orientation, undefined);
  assert.equal(unsafe.touch_mappers[0].orientation_present, true);
  assert.doesNotMatch(JSON.stringify(unsafe), /private/);
});

test("build evidence keeps SDK and emulator identity without installation paths", () => {
  const summary = summarizeBuildIdentity({
    apiLevel: "35\n", release: "15\n", buildId: "AE3A.240806.005\n",
    fingerprint: "Android/sdk_gphone64_x86_64/emulator64_x86_64:15/AE3A.240806.005/123456:userdebug/dev-keys\n",
    emulator: "Android emulator version 37.2.13.0 (build_id 16740729) (CL:N/A)\nprivate-details\n",
    adb: "Android Debug Bridge version 1.0.41\nVersion 36.0.2-14143358\nInstalled as /private/path/adb\n",
  });
  assert.equal(summary.android_api_level, 35);
  assert.equal(summary.android_release, "15");
  assert.equal(summary.android_build_id, "AE3A.240806.005");
  assert.equal(summary.emulator_version, "37.2.13.0");
  assert.equal(summary.emulator_build_id, "16740729");
  assert.equal(summary.adb_protocol_version, "1.0.41");
  assert.equal(summary.adb_build_version, "36.0.2-14143358");
  assert.doesNotMatch(JSON.stringify(summary), /private/);
  const malformed = summarizeBuildIdentity({ apiLevel: "35 private-value", buildId: "unexpected private-value", emulator: "private-output" });
  assert.equal(malformed.android_api_level, undefined);
  assert.equal(malformed.android_build_id, undefined);
  assert.equal(malformed.emulator_version, undefined);
  assert.doesNotMatch(JSON.stringify(malformed), /private/);
});

test("ANR owner requires real system controls and exact whitelisted title evidence", () => {
  const nodes = (title) => [
    { package: "android", "resource-id": "android:id/aerr_wait", text: "Wait" },
    { package: "android", "resource-id": "android:id/alertTitle", text: title },
  ];
  assert.equal(classifyAnrOwner(nodes("System UI isn&apos;t responding")), "system_ui");
  assert.equal(classifyAnrOwner(nodes("System UI isn&#x2019;t responding")), "system_ui");
  assert.equal(classifyAnrOwner(nodes("Android MCP Acceptance isn't responding")), "acceptance_fixture");
  assert.equal(classifyAnrOwner(nodes("Launcher3 isn't responding")), "other");
  assert.equal(classifyAnrOwner(nodes("x".repeat(26))), "unknown");
  assert.equal(classifyAnrOwner(nodes("private-app isn't responding")), "unknown");
  assert.equal(classifyAnrOwner(nodes("System UI isn't responding").slice(1)), undefined);
  assert.equal(classifyAnrOwner(nodes("System UI isn't responding").map((node) => ({ ...node, package: "private-app" }))), undefined);
  const ambiguous = [...nodes("System UI isn't responding"), { package: "android", "resource-id": "android:id/alertTitle", text: "Launcher3 isn't responding" }];
  assert.equal(classifyAnrOwner(ambiguous), "unknown");
});

test("verification summaries bound recovery evidence and omit text and selector contents", () => {
  const summary = summarizeVerification({
    requested: true, satisfied: false, timed_out: true, elapsed_ms: 30000,
    target_text_matches_requested: false,
    target_selector: { text: "private-text" }, wait_for_selector: { text: "private-other-text" },
    handled_system_dialogs: Array.from({ length: 10 }, () => ({ kind: "anr", action: "wait", labels: ["private-label"] })),
  });
  assert.equal(summary.target_text_matches_requested, false);
  assert.equal(summary.handled_system_dialogs.length, 4);
  assert.deepEqual(summary.handled_system_dialogs[0], { kind: "anr", action: "wait" });
  assert.doesNotMatch(JSON.stringify(summary), /private/);
});
