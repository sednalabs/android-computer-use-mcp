import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { summarizeDisplayGeometry, summarizeInputGeometry, summarizeWmSize } from "./android_provider_geometry.mjs";
import {
  awakeUnlockedReadiness, classifyAnrOwner, pollReadOnlyReadiness,
  summarizeAnrTraces, summarizeBuildIdentity, summarizeCredentialReadiness,
  summarizeCurrentAnrWindow, summarizeEmulatorVersionProbe, summarizeVerification,
} from "./android_provider_diagnostics.mjs";

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
  const logged = summarizeBuildIdentity({ emulator: "INFO         | Android emulator version 37.2.13.0 (build_id 16740729) (CL:N/A)\nprivate-details" });
  assert.equal(logged.emulator_version, "37.2.13.0");
  assert.equal(logged.emulator_build_id, "16740729");
  assert.doesNotMatch(JSON.stringify(logged), /private/);
  const malformed = summarizeBuildIdentity({ apiLevel: "35 private-value", buildId: "unexpected private-value", emulator: "private-output" });
  assert.equal(malformed.android_api_level, undefined);
  assert.equal(malformed.android_build_id, undefined);
  assert.equal(malformed.emulator_version, undefined);
  assert.doesNotMatch(JSON.stringify(malformed), /private/);
});

test("a nonzero version probe retains a valid stderr identity and truthful exit status", () => {
  const result = spawnSync(process.execPath, ["-e", `
    process.stdout.write("private-startup-output\\n".repeat(150));
    process.stderr.write("INFO         | Android emulator version 37.2.13.0 (build_id 16740729) (CL:N/A)\\nprivate-error\\n");
    process.exitCode = 7;
  `], { timeout: 5000, maxBuffer: 64 * 1024 });
  const summary = summarizeEmulatorVersionProbe(result);
  assert.equal(summary.emulator_version, "37.2.13.0");
  assert.equal(summary.emulator_build_id, "16740729");
  assert.deepEqual(summary.emulator_version_probe, {
    capture_status: "nonzero_exit", exit_code: 7, termination_signal: null,
    parse_status: "matched", version_streams: ["stderr"], truncated_streams: [],
  });
  assert.doesNotMatch(JSON.stringify(summary), /private/);
});

test("version probe parsing handles bounded banners and rejects conflicting identities", () => {
  const line = "Android emulator version 37.2.13.0 (build_id 16740729)";
  const summary = summarizeEmulatorVersionProbe({ status: 0, stdout: `${"private-banner\n".repeat(300)}${line}\n`, stderr: `INFO | ${line} (CL:N/A)\n` });
  assert.equal(summary.emulator_version, "37.2.13.0");
  assert.equal(summary.emulator_version_probe.capture_status, "completed");
  assert.deepEqual(summary.emulator_version_probe.version_streams, ["stdout", "stderr"]);
  assert.doesNotMatch(JSON.stringify(summary), /private/);
  const conflict = summarizeEmulatorVersionProbe({ status: 0, stdout: line, stderr: line.replace("37.2.13.0", "37.2.14.0") });
  assert.equal(conflict.emulator_version, undefined);
  assert.equal(conflict.emulator_version_probe.parse_status, "ambiguous");
  const beyondBound = summarizeEmulatorVersionProbe({ status: 0, stdout: `${"x".repeat(64 * 1024)}\n${line}\n` });
  assert.equal(beyondBound.emulator_version, undefined);
  assert.deepEqual(beyondBound.emulator_version_probe.truncated_streams, ["stdout"]);
});

test("version probe failures expose only categorical status and exact known fields", () => {
  for (const [code, status] of [["ETIMEDOUT", "timeout"], ["ENOBUFS", "output_limit"], ["ENOENT", "not_found"], ["EACCES", "permission_denied"], ["private-error", "other_error"]]) {
    const summary = summarizeEmulatorVersionProbe({ status: null, signal: "SIGTERM", error: { code, message: "private-error-path" }, stderr: "private-output" });
    assert.equal(summary.emulator_version_probe.capture_status, status);
    assert.equal(summary.emulator_version_probe.exit_code, null);
    assert.equal(summary.emulator_version_probe.termination_signal, "SIGTERM");
    assert.equal(summary.emulator_version_probe.parse_status, "missing");
    assert.doesNotMatch(JSON.stringify(summary), /private/);
  }
  const signaled = summarizeEmulatorVersionProbe({ status: null, signal: "SIGABRT", stdout: "Android emulator version 37.2.13.0 (build_id 16740729)\n" });
  assert.equal(signaled.emulator_version, "37.2.13.0");
  assert.equal(signaled.emulator_version_probe.capture_status, "signaled");
  assert.equal(signaled.emulator_version_probe.termination_signal, "SIGABRT");
  assert.equal(summarizeEmulatorVersionProbe({ status: null, signal: "private-signal" }).emulator_version_probe.termination_signal, "other");
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

const anrWindow = (process = "com.android.systemui") => `  Window #0 Window{a123 u0 Application Not Responding: ${process}}:
    mOwnerUid=1000 showForAllUsers=true package=android appop=NONE
    mAttrs={private-window-details}
    mHasSurface=true isReadyForDisplay()=true mWindowRemovalAllowed=false
    isVisible=true
`;

test("current ANR window owner requires one active system window and observed controls", () => {
  const summary = summarizeCurrentAnrWindow(anrWindow(), true);
  assert.deepEqual(summary, {
    available: true, system_controls_observed: true, anr_window_count: 1,
    active_anr_window_count: 1, process_category: "system_ui", owner: "system_ui",
  });
  assert.equal(summarizeCurrentAnrWindow(anrWindow("com.sednalabs.mcp.acceptance"), true).owner, "acceptance_fixture");
  assert.equal(summarizeCurrentAnrWindow(anrWindow("com.android.launcher3"), true).owner, "other");
  assert.equal(summarizeCurrentAnrWindow(anrWindow(), false).owner, "unknown");
  assert.equal(summarizeCurrentAnrWindow(`  mCurrentFocus=Window{a123 u0 Application Not Responding: com.android.systemui}\n`, true).owner, "unknown");
  assert.doesNotMatch(JSON.stringify(summary), /private|a123|com\.android|1000/);
});

test("current ANR window owner ignores proved inactive windows and rejects ambiguity", () => {
  for (const inactive of [
    anrWindow().replace("com.android.systemui}", "com.android.systemui EXITING}"),
    anrWindow().replace("isVisible=true", "isVisible=false"),
    anrWindow().replace("isReadyForDisplay()=true", "isReadyForDisplay()=false"),
    `${anrWindow()}    mAnimatingExit=false mRemoveOnExit=false mDestroying=true mRemoved=false\n`,
  ]) {
    assert.equal(summarizeCurrentAnrWindow(inactive, true).owner, "unknown");
    assert.equal(summarizeCurrentAnrWindow(`${inactive}${anrWindow("com.sednalabs.mcp.acceptance")}`, true).owner, "acceptance_fixture");
  }
  for (const ambiguous of [
    anrWindow().repeat(2),
    anrWindow().replace("mOwnerUid=1000", "mOwnerUid=10123"),
    anrWindow().replace("package=android", "package=private-app"),
    anrWindow().replace("    isVisible=true\n", ""),
    anrWindow().replace("    isVisible=true\n", "    isVisible=true\n    isVisible=false\n"),
    anrWindow().replace("    mHasSurface=true", "      mHasSurface=true"),
    `${anrWindow()}${anrWindow("private-process")}`,
  ]) assert.equal(summarizeCurrentAnrWindow(ambiguous, true).owner, "unknown");
});

test("ANR window summaries discard unknown process names and reject oversized input", () => {
  const unknown = summarizeCurrentAnrWindow(anrWindow("private-process"), true);
  assert.equal(unknown.process_category, "unknown");
  assert.doesNotMatch(JSON.stringify(unknown), /private|a123|1000/);
  for (const dump of [anrWindow().repeat(129), "x".repeat(8193), "\n".repeat(32769), "x\n".repeat(1024 * 1024 + 1)]) {
    assert.deepEqual(summarizeCurrentAnrWindow(dump, true), { available: false, reason: "bounds_exceeded" });
  }
  assert.deepEqual(summarizeCurrentAnrWindow(undefined, true), { available: false, reason: "unavailable" });
});

const anrTrace = (process = "com.android.systemui", pid = "123") => `----- pid ${pid} at 2026-11-19 00:00:00 -----
Cmd line: ${process}
Original command line: private-command
Build fingerprint: private-fingerprint
"main" prio=5 tid=1 Native
  | sysTid=123 private-thread-detail
  at libcore.io.Linux.write(Native method)
  at libcore.io.IoBridge.write(IoBridge.java:123)
  at android.view.ViewDebug.dumpEncoded(ViewDebug.java:456)
  at private.app.Secret.method(Secret.java:789)
  at android.os.Looper.loop(Looper.java:99)

"Binder:123_A" prio=5 tid=3 Runnable
  at android.view.ViewRootImpl$W.executeCommand(ViewRootImpl.java:12)
  at android.view.ViewDebug.dispatchCommand(ViewDebug.java:34)

"private-thread-name" prio=5 tid=4 Waiting
  at private.app.Hidden.method(Hidden.java:999)
----- end ${pid} -----
`;

test("historical ANR trace summaries retain only known process categories and frame symbols", () => {
  const summary = summarizeAnrTraces(`ACTIVITY MANAGER LAST ANR TRACES\nFile: /private/path\nSubject: Input dispatching timed out (private-reason)\n${anrTrace()}${anrTrace("private.process", "456")}`);
  assert.equal(summary.relation, "latest_anr_directory_file");
  assert.equal(summary.reason_category, "input_dispatch");
  assert.equal(summary.processes.length, 1);
  assert.equal(summary.processes_omitted, true);
  assert.equal(summary.processes[0].process_category, "system_ui");
  assert.equal(summary.processes[0].complete, true);
  assert.equal(summary.processes[0].frames_allowlisted_only, true);
  assert.deepEqual(summary.processes[0].threads, [
    { category: "main", state: "Native", known_frames: ["libcore.io.Linux.write", "libcore.io.IoBridge.write", "android.view.ViewDebug.dumpEncoded", "android.os.Looper.loop"] },
    { category: "binder", state: "Runnable", known_frames: ["android.view.ViewRootImpl$W.executeCommand", "android.view.ViewDebug.dispatchCommand"] },
  ]);
  assert.doesNotMatch(JSON.stringify(summary), /private|123|456|789|\.java|com\.android\.systemui|2026/);
});

test("trace metadata is scoped to exact process and thread boundaries", () => {
  const duplicate = anrTrace().replace("Cmd line: com.android.systemui", "Cmd line: com.android.systemui\nCmd line: private.process");
  assert.equal(summarizeAnrTraces(duplicate).processes.length, 0);
  const unframed = anrTrace().replace(/^----- pid[^\n]*\n/, "");
  assert.equal(summarizeAnrTraces(unframed).processes.length, 0);
  const trailing = `${anrTrace()}"main" prio=5 tid=1 Blocked\n  at android.view.View.encode(View.java:1)\n`;
  assert.doesNotMatch(JSON.stringify(summarizeAnrTraces(trailing)), /android\.view\.View\.encode/);
  const unknownThread = anrTrace().replace('"Binder:123_A" prio=5 tid=3 Runnable', '"private-thread" malformed');
  assert.doesNotMatch(JSON.stringify(summarizeAnrTraces(unknownThread)), /executeCommand|private/);
  const partial = summarizeAnrTraces(anrTrace().replace("----- end 123 -----", ""));
  assert.equal(partial.processes.length, 0);
  assert.equal(partial.incomplete_process_sections, true);
  assert.equal(summarizeAnrTraces(anrTrace().replace("----- end 123 -----", "----- end 456 -----")).processes.length, 0);
});

test("ANR reasons and thread states use static categories and discard arbitrary detail", () => {
  for (const [reason, expected] of [
    ["private-window is not responding. Waited 5000ms for FocusEvent(hasFocus=true)", "input_dispatch"],
    ["private-app does not have a focused window", "input_dispatch_no_focused_window"],
    ["Broadcast of Intent { private-payload }", "broadcast_receiver"],
    ["executing service private-service, waited 100ms", "service_execution"],
    ["Context.startForegroundService() did not then call Service.startForeground(): private-service", "service_start"],
    ["private-reason", "unknown"],
  ]) {
    const summary = summarizeAnrTraces(`Subject: ${reason}\n${anrTrace().replace('tid=1 Native', 'tid=1 WaitingForGcToComplete')}`);
    assert.equal(summary.reason_category, expected);
    assert.equal(summary.processes[0].threads[0].state, "WaitingForGcToComplete");
    assert.doesNotMatch(JSON.stringify(summary), /private/);
  }
  assert.equal(summarizeAnrTraces(`Subject: Broadcast of private\nSubject: executing service private\n${anrTrace()}`).reason_category, "unknown");
  assert.equal(summarizeAnrTraces(anrTrace().replace('tid=1 Native', 'tid=1 PrivateState')).processes[0].threads[0].state, "unknown");
});

test("ANR trace summaries bound output and prioritize main and observer stacks", () => {
  const binder = '"Binder:123_B" prio=5 tid=7 Native\n  at android.os.Binder.execTransact(Binder.java:1)\n';
  const busy = anrTrace().replace('"main"', `${binder.repeat(20)}"main"`);
  const summary = summarizeAnrTraces(busy);
  assert.equal(summary.processes[0].threads.length, 12);
  assert.equal(summary.processes[0].threads[0].category, "main");
  assert.ok(summary.processes[0].threads[1].known_frames.includes("android.view.ViewRootImpl$W.executeCommand"));
  assert.equal(summary.processes[0].threads_omitted, true);
  assert.equal(summarizeAnrTraces(anrTrace().repeat(9)).processes.length, 8);
  assert.equal(summarizeAnrTraces(anrTrace().repeat(9)).processes_omitted, true);
  for (const dump of [anrTrace().repeat(65), "x".repeat(8193), "\n".repeat(32769), "x\n".repeat(1024 * 1024 + 1)]) {
    assert.deepEqual(summarizeAnrTraces(dump), { available: false, reason: "bounds_exceeded" });
  }
  assert.deepEqual(summarizeAnrTraces(undefined), { available: false, reason: "unavailable" });
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

const lockDump = (type = "NONE", started = true) => `Current lock settings service state:
User State:
  User 0
    LSKF-based SP protector ID: private-protector-id
    SID: private-security-id
    CredentialType: ${type}
    SeparateChallenge: false
Keys in namespace:
  private-key-alias
ThirdPartyAppsStarted: ${started}
`;
const uiState = (showing = "false", wakefulness = "Awake") => ({
  power: { available: true, fields: [{ name: "mWakefulness", value: wakefulness }] },
  window_policy: { available: true, fields: [
    { name: "showing", value: showing }, { name: "inputRestricted", value: "false" },
    { name: "secure", value: "true" }, // Conservative startup cache; not credential proof.
  ] },
});

test("readiness requires initialized current-user NONE even when the policy cache says secure", () => {
  const summary = summarizeCredentialReadiness(lockDump(), 0);
  assert.deepEqual(summary, { current_user: 0, state: "ready", credential_type: "NONE", third_party_apps_started: true });
  assert.equal(awakeUnlockedReadiness(uiState()), "ready");
  assert.doesNotMatch(JSON.stringify(summary), /private|protector|SID|alias/);
});

test("credential readiness rejects wrong or missing users and ambiguous fields", () => {
  for (const [dump, user] of [
    [lockDump(), 10], [lockDump(), undefined], [undefined, 0],
    [lockDump().replace("    CredentialType: NONE\n", ""), 0],
    [lockDump().replace("    CredentialType: NONE", "    CredentialType: NONE\n    CredentialType: NONE"), 0],
    [lockDump().replace("Keys in namespace:", "  User 0\n    CredentialType: NONE\nKeys in namespace:"), 0],
    [lockDump().replace("ThirdPartyAppsStarted: true", ""), 0],
    [`${lockDump()}ThirdPartyAppsStarted: true\n`, 0],
    [`${lockDump()}${"\n".repeat(8192)}ThirdPartyAppsStarted: false\n`, 0],
    [lockDump().replace("    CredentialType: NONE", `    CredentialType: NONE\n    CredentialType: ${"X".repeat(2048)}`), 0],
    [lockDump().replace("ThirdPartyAppsStarted: true", "  ThirdPartyAppsStarted: true"), 0],
    [lockDump().replace("  User 0", "  User 10\n    User 0").replace("    CredentialType: NONE", "      CredentialType: NONE"), 0],
    [lockDump().replace("    CredentialType: NONE", "      CredentialType: NONE"), 0],
    [lockDump().replace("User State:", "  User State:"), 0],
  ]) {
    assert.equal(summarizeCredentialReadiness(dump, user).state, "failed");
  }
  const twoUsers = lockDump().replace("Keys in namespace:", "  User 10\n    CredentialType: PIN\nKeys in namespace:");
  assert.equal(summarizeCredentialReadiness(twoUsers, 10).state, "failed");
  assert.equal(summarizeCredentialReadiness(twoUsers, 10).credential_type, "PIN");
});

test("PIN, password, pattern and unknown credentials never become pending or ready", () => {
  for (const type of ["PIN", "PASSWORD", "PATTERN", "UNKNOWN_9", "None"]) {
    for (const started of [true, false]) {
      assert.equal(summarizeCredentialReadiness(lockDump(type, started), 0).state, "failed");
    }
  }
  assert.equal(summarizeCredentialReadiness(lockDump("NONE", false), 0).state, "pending");
});

test("read-only readiness polling accepts a known initialization transition", async () => {
  let time = 0;
  let reads = 0;
  const result = await pollReadOnlyReadiness(() => summarizeCredentialReadiness(lockDump("NONE", reads++ > 0), 0), (state) => state.state, {
    timeoutMs: 1000, intervalMs: 250, now: () => time, wait: async (ms) => { time += ms; },
  });
  assert.equal(result.ready, true);
  assert.equal(result.polls, 2);
  assert.equal(result.elapsed_ms, 250);
  assert.equal(result.last.credential_type, "NONE");
});

test("read-only polling bounds persistent initialization and fails unknown or secured states immediately", async () => {
  for (const type of ["PIN", "PASSWORD", "PATTERN", "UNKNOWN_9"]) {
    const result = await pollReadOnlyReadiness(() => summarizeCredentialReadiness(lockDump(type), 0), (state) => state.state, {
      wait: async () => assert.fail("Secured or unknown state must fail before waiting or input."),
    });
    assert.equal(result.ready, false);
    assert.equal(result.polls, 1);
    assert.equal(result.timed_out, false);
  }
  let time = 0;
  const result = await pollReadOnlyReadiness(() => summarizeCredentialReadiness(lockDump("NONE", false), 0), (state) => state.state, {
    timeoutMs: 500, intervalMs: 250, now: () => time, wait: async (ms) => { time += ms; },
  });
  assert.equal(result.ready, false);
  assert.equal(result.timed_out, true);
  assert.equal(result.elapsed_ms, 500);
  assert.equal(result.polls, 2);
});

test("UI readiness requires explicit awake, not showing and unrestricted input", async () => {
  assert.equal(awakeUnlockedReadiness(uiState("true")), "pending");
  assert.equal(awakeUnlockedReadiness(uiState("false", "Asleep")), "pending");
  assert.equal(awakeUnlockedReadiness({ power: { available: false } }), "failed");
  const ambiguous = uiState();
  ambiguous.window_policy.fields.push({ name: "showing", value: "false" });
  assert.equal(awakeUnlockedReadiness(ambiguous), "failed");
  let reads = 0;
  let time = 0;
  const result = await pollReadOnlyReadiness(() => uiState(reads++ ? "false" : "true"), awakeUnlockedReadiness, {
    timeoutMs: 1000, intervalMs: 250, now: () => time, wait: async (ms) => { time += ms; },
  });
  assert.equal(result.ready, true);
  assert.equal(result.polls, 2);
});
