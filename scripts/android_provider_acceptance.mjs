#!/usr/bin/env node
// Real-device gate. A missing emulator, hosted artifact or serial-bound gRPC endpoint fails the run.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, open, readFile, realpath, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { createMcpStreamableHttpClient } from "../adapters/openai/mcp_streamable_http_client.js";
import { assertTypedPng } from "./codex_journey_assertions.mjs";
import { summarizeDisplayGeometry, summarizeInputGeometry, summarizeWmSize } from "./android_provider_geometry.mjs";
import {
  awakeUnlockedReadiness, classifyAnrOwner, decodeXmlAttribute, pollReadOnlyReadiness,
  summarizeBuildIdentity, summarizeCredentialReadiness, summarizeVerification,
} from "./android_provider_diagnostics.mjs";

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const required = (name) => {
  const value = process.env[name];
  assert.ok(value, `${name} is required for the real-device gate.`);
  return value;
};
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
const sdk = required("ANDROID_SDK_ROOT");
const serial = required("ANDROID_SERIAL");
assert.match(serial, /^emulator-[0-9]+$/, "This gate requires the disposable Android emulator.");
const adb = path.join(sdk, "platform-tools/adb");
const binary = process.env.MCP_BINARY || path.join(repo, "target/debug/android-computer-use-mcp");
const artifactName = required("ANDROID_ACCEPTANCE_ARTIFACT_NAME");
const sourceSha = required("ANDROID_ACCEPTANCE_SOURCE_SHA");
const archiveDigest = required("ANDROID_ACCEPTANCE_ARTIFACT_SHA256").replace(/^sha256:/, "");
const repository = required("GITHUB_REPOSITORY");
const runId = Number(required("GITHUB_RUN_ID"));
assert.ok(Number.isSafeInteger(runId) && runId > 0);
assert.match(sourceSha, /^[a-f0-9]{40}$/);
assert.match(archiveDigest, /^[a-f0-9]{64}$/);
const packageName = "com.sednalabs.mcp.acceptance";
const activityName = `${packageName}.MainActivity`;
const endpoint = new URL(process.env.MCP_ENDPOINT || "http://127.0.0.1:9526/mcp");
assert.equal(endpoint.protocol, "http:");
assert.ok(["localhost", "127.0.0.1", "[::1]"].includes(endpoint.hostname));
assert.ok(!endpoint.username && !endpoint.password);
const health = new URL("/health", endpoint);
const temporaryRoot = required("RUNNER_TEMP");
const evidencePath = process.env.ANDROID_PROVIDER_EVIDENCE || path.join(temporaryRoot, "android-provider-acceptance.json");
await rm(evidencePath, { force: true });
const work = await mkdtemp(path.join(temporaryRoot, "android-provider-acceptance-"));
const artifacts = path.join(work, "artifacts");
const hostedSession = path.join(work, "hosted-session");
await mkdir(artifacts, { mode: 0o700 });
await mkdir(hostedSession, { mode: 0o700 });
const log = await open(path.join(work, "server.log"), "wx", 0o600);
const checks = [];
const geometryEvidence = [];
let currentGate = "start";
let server;
let client;
let installEvidence;
let deviceReadiness;
let deviceIdentity;
const started = Date.now();
const adbCall = (args, allowNonzero = false) => {
  const result = spawnSync(adb, ["-s", serial, ...args], { timeout: 20000, maxBuffer: 32 * 1024 * 1024 });
  assert.ok(!result.error, `ADB evidence collection failed during ${currentGate}.`);
  if (!allowNonzero) assert.equal(result.status, 0, `ADB evidence collection failed during ${currentGate}.`);
  return result.stdout;
};
const adbText = (args, allowNonzero = false) => adbCall(args, allowNonzero).toString("utf8").trim();
const id = (name) => ({ resource_id: `${packageName}:id/${name}` });
const text = (value) => ({ text: value, text_exact: true });
const fixtureLabels = new Set([
  "Android MCP acceptance", "Increment", "Acceptance text input", "Enter test text",
  "Open dialog", "Acceptance dialog", "Clipboard preserved", "Clipboard changed", "Close dialog",
  "Open scroll page", "Scroll acceptance page", "Scroll destination reached",
  "Back to controls", "Touch pad", "Acceptance touch pad", "Sedna ASCII", "Sedna café Ω %s",
  "System UI isn't responding", "System UI isn’t responding",
  "Pixel Launcher isn't responding", "Launcher3 isn't responding",
  "Android MCP Acceptance isn't responding", "Wait", "Close app",
]);
const staticFixtureLabel = (label) => typeof label === "string" &&
  (fixtureLabels.has(label) || /^(?:Count: \d{1,3}|Max pointers: [0-5]|Completed two-pointer gestures: \d{1,3}|Scroll row \d{1,2})$/.test(label))
  ? label : undefined;
function deviceState(timeoutMs = 10000) {
  const deadline = performance.now() + timeoutMs;
  const capture = (args) => {
    const remaining = Math.ceil(deadline - performance.now());
    if (remaining <= 0) return { available: false };
    const result = spawnSync(adb, ["-s", serial, "shell", "dumpsys", ...args], { timeout: Math.min(5000, remaining), maxBuffer: 1024 * 1024 });
    if (result.error || result.status !== 0) return { available: false };
    const pattern = /\b(mWakefulness|mInteractive|mAwake|mScreenOnEarly|mScreenOnFully|mKeyguardShowing|mKeyguardOccluded|showing|showingAndNotOccluded|inputRestricted|occluded|secure|deviceHasKeyguard)\s*[:=]\s*(true|false|Awake|Asleep|Dreaming|Dozing)\b/g;
    const fields = [...result.stdout.toString("utf8").matchAll(pattern)].map((match) => ({ name: match[1], value: match[2] }));
    return fields.length > 40 ? { available: false } : { available: true, fields };
  };
  return { power: capture(["power"]), window_policy: capture(["window", "policy"]) };
}
function credentialReadiness(timeoutMs) {
  const deadline = performance.now() + timeoutMs;
  const capture = (args) => {
    const remaining = Math.ceil(deadline - performance.now());
    if (remaining <= 0) return undefined;
    const result = spawnSync(adb, ["-s", serial, "shell", ...args], { timeout: Math.min(5000, remaining), maxBuffer: 1024 * 1024 });
    return result.error || result.status !== 0 ? undefined : result.stdout.toString("utf8");
  };
  const currentUser = () => {
    const value = capture(["am", "get-current-user"])?.trim();
    return /^(?:0|[1-9]\d{0,5})$/.test(value || "") ? Number(value) : undefined;
  };
  const user = currentUser();
  if (user === undefined) return { state: "failed" };
  const result = summarizeCredentialReadiness(capture(["dumpsys", "lock_settings"]), user);
  if (result.state !== "failed" && currentUser() !== user) result.state = "failed";
  return result;
}
function recordDeviceGeometry(phase) {
  const capture = (args, summarize) => {
    const result = spawnSync(adb, ["-s", serial, "shell", ...args], { timeout: 5000, maxBuffer: 1024 * 1024 });
    if (result.error || result.status !== 0) return { available: false };
    return { available: true, ...summarize(result.stdout.toString("utf8")) };
  };
  geometryEvidence.push({
    phase,
    wm_size: capture(["wm", "size"], summarizeWmSize),
    input: capture(["dumpsys", "input"], summarizeInputGeometry),
    display: capture(["dumpsys", "display"], summarizeDisplayGeometry),
  });
}
function deviceBuildIdentity() {
  const capture = (command, args, includeStderr = false) => {
    const result = spawnSync(command, args, { timeout: 5000, maxBuffer: 64 * 1024 });
    return result.error || result.status !== 0 ? "" : result.stdout.toString("utf8")
      + (includeStderr ? `\n${result.stderr.toString("utf8")}` : "");
  };
  const property = (name) => capture(adb, ["-s", serial, "shell", "getprop", name]);
  return summarizeBuildIdentity({
    apiLevel: property("ro.build.version.sdk"), release: property("ro.build.version.release"),
    buildId: property("ro.build.id"), fingerprint: property("ro.build.fingerprint"),
    emulator: capture(path.join(sdk, "emulator/emulator"), ["-version"], true),
    adb: capture(adb, ["version"]),
  });
}
async function hierarchyEvidence(value) {
  const candidates = [
    ...((value.actions || []).filter((row) => row.ok === false).flatMap((row) => {
      const data = row.error?.data || row.receipt?.error?.data;
      return [
        { source: "text_verification", path: data?.artifacts?.post_text_hierarchy_path },
        { source: "tap_verification", path: data?.artifacts?.post_tap_hierarchy_path },
        { source: "postcondition", path: (row.receipt?.postcondition || data?.postcondition)?.artifacts?.hierarchy_path },
      ];
    })),
    { source: "final_observation", path: value.observation?.artifacts?.hierarchy_path },
  ].filter((candidate) => typeof candidate.path === "string").slice(0, 2);
  const summaries = [];
  for (const candidate of candidates) {
    let handle;
    try {
      const resolved = await realpath(candidate.path);
      const relative = path.relative(await realpath(artifacts), resolved);
      assert.ok(relative.startsWith(`uiautomator${path.sep}`) && !path.isAbsolute(relative));
      handle = await open(resolved, "r");
      assert.ok((await handle.stat()).size <= 1024 * 1024, "Hierarchy evidence is bounded.");
      const xml = await handle.readFile("utf8");
      const nodes = [...xml.matchAll(/<node\b([^>]*)>/g)];
      const attributes = nodes.slice(0, 64).map((match) =>
        Object.fromEntries([...match[1].matchAll(/([\w:-]+)="([^"]*)"/g)].map((attr) => [attr[1], attr[2]])),
      );
      summaries.push({ source: candidate.source, node_count: nodes.length, anr_owner: classifyAnrOwner(attributes), nodes: attributes.map((attrs) => {
        return {
          package_name: attrs.package?.slice(0, 160), class_name: attrs.class?.slice(0, 160),
          resource_id: attrs["resource-id"]?.slice(0, 200), label: staticFixtureLabel(decodeXmlAttribute(attrs.text)),
          content_description: staticFixtureLabel(decodeXmlAttribute(attrs["content-desc"])),
          text_length: attrs.text?.length, bounds: attrs.bounds?.slice(0, 80),
          enabled: attrs.enabled, clickable: attrs.clickable, focused: attrs.focused,
        };
      }) });
    } catch { summaries.push({ source: candidate.source, available: false }); }
    finally { if (handle) await handle.close().catch(() => {}); }
  }
  return summaries;
}
function receipt(result) {
  if (!Array.isArray(result?.content)) return result;
  const texts = result.content.filter((part) => part.type === "text");
  assert.equal(texts.length, 1, "Image-bearing results require one JSON text receipt.");
  return JSON.parse(texts[0].text);
}
function imageEvidence(result) {
  assert.equal(result.structuredContent, undefined, "Structured content would suppress consumer images.");
  const images = result.content?.filter((part) => part.type === "image") || [];
  assert.equal(images.length, 1, "Expected one typed image from the real emulator.");
  assert.equal(images[0].mimeType, "image/png");
  return assertTypedPng([{ type: "input_image", detail: "high", image_url: `data:image/png;base64,${images[0].data}` }]);
}
function handledDialogs(condition) {
  return condition?.handled_system_dialogs?.slice(0, 4).map((dialog) => ({
    kind: dialog.kind, action: dialog.action,
  })) || [];
}
function failureSummary(value) {
  // Keep actionable errors while excluding screenshots, hierarchies, transport
  // stdout/stderr, request arguments and arbitrary receipt fields from evidence.
  const errorSummary = (error) => typeof error === "string"
    ? { message: error.slice(0, 1500) }
    : error && {
      code: error.code, kind: error.kind,
      message: typeof error.message === "string" ? error.message.slice(0, 1500) : undefined,
      outcome: error.data?.outcome, mutation_dispatched: error.data?.mutation_dispatched,
      phase: error.data?.phase, paste_attempted: error.data?.paste_attempted,
      text_verified: error.data?.text_verified, clipboard_restore: error.data?.clipboard_restore,
    };
  const conditionSummary = (condition) => condition && {
    requested: condition.requested, satisfied: condition.satisfied,
    timed_out: condition.timed_out, elapsed_ms: condition.elapsed_ms,
    evidence_source: condition.evidence_source, match_count: condition.match_count,
    observed_package: condition.observed_package, observed_activity: condition.observed_activity,
    handled_system_dialogs: handledDialogs(condition),
  };
  return {
    outcome: value.outcome, executed_action_count: value.executed_action_count,
    attempted_action_count: value.attempted_action_count, failed_index: value.failed_index,
    retry_safe: value.retry_safe, do_not_replay: value.do_not_replay,
    error: errorSummary(value.error),
    actions: value.actions?.slice(0, 16).map((row) => ({
      index: row.index, tool: row.tool, outcome: row.outcome, ok: row.ok,
      elapsed_ms: row.elapsed_ms, error: errorSummary(row.error || row.receipt?.error),
      postcondition: conditionSummary(row.receipt?.postcondition || row.error?.data?.postcondition),
      verification: summarizeVerification(row.receipt?.verification || row.error?.data?.verification),
      focus_verification: summarizeVerification(row.receipt?.focus_verification || row.error?.data?.focus_verification),
      text_verification: summarizeVerification(row.receipt?.text_verification || row.error?.data?.text_verification),
    })),
    observation: value.observation && {
      screenshot_present: value.observation.screenshot_present,
      ui_available: value.observation.ui_available, elapsed_ms: value.observation.elapsed_ms,
      node_count: value.observation.node_count,
      visible_node_count: value.observation.visible_ui?.total_labeled_count,
      notes: value.observation.notes?.slice(0, 4).map((note) => String(note).slice(0, 200)),
    },
  };
}
async function call(name, args, { ok = true, image = false } = {}) {
  currentGate = args.action_id ? `${name}:${args.action_id}` : name;
  const begin = Date.now();
  const result = await client.callTool(name, args);
  const value = receipt(result);
  const check = { tool: name, action_id: args.action_id, ok: value.ok, expected_ok: ok, elapsed_ms: Date.now() - begin };
  if (image && value.observation) {
    const observation = value.observation;
    check.observation = {
      screenshot_backend: observation.artifacts?.screenshot_backend || observation.screenshot_receipt?.backend_used,
      width: observation.width, height: observation.height,
      rotation_quarter_turns: observation.rotation_quarter_turns,
      coordinate_actions_available: observation.coordinate_actions_available,
    };
  }
  const conditions = (entry) => [entry?.postcondition, entry?.verification, entry?.focus_verification, entry?.text_verification];
  const recoveredDialogs = [...conditions(value), ...(value.actions || []).flatMap((row) =>
    [...conditions(row.receipt), ...conditions(row.error?.data)],
  )].flatMap(handledDialogs).slice(0, 16);
  if (recoveredDialogs.length) check.handled_system_dialogs = recoveredDialogs;
  checks.push(check);
  if (value.ok !== ok) {
    check.failure = failureSummary(value);
    check.failure.device_state = deviceState();
    check.failure.hierarchies = await hierarchyEvidence(value);
    const anrOwners = [...new Set(check.failure.hierarchies.map((hierarchy) => hierarchy.anr_owner).filter(Boolean))];
    if (anrOwners.length) check.failure.anr_owner = anrOwners.length === 1 ? anrOwners[0] : "unknown";
    console.error(JSON.stringify({ failed_gate: currentGate, failure: check.failure }));
  }
  assert.equal(value.ok, ok, `Unexpected outcome for ${currentGate}.`);
  for (const dialog of recoveredDialogs) {
    assert.deepEqual(dialog, { kind: "anr", action: "wait" }, "Fixture recovery must use the bounded safe ANR Wait policy.");
  }
  if (image) assert.equal(Boolean(result.isError), !ok, `${name} MCP error flag must match the receipt.`);
  const imageReceipt = image ? imageEvidence(result) : undefined;
  if (imageReceipt) {
    check.image = imageReceipt;
    assert.equal(imageReceipt.width, value.observation.width, "Receipt width must match decoded PNG pixels.");
    assert.equal(imageReceipt.height, value.observation.height, "Receipt height must match decoded PNG pixels.");
  }
  return value;
}
async function step(name, actions, extra = {}) {
  const value = await call("android.step", {
    serial, action_id: `provider-acceptance-${name}`, actions, ...extra,
  }, { image: true });
  assert.equal(value.executed_action_count, actions.length);
  assert.equal(value.attempted_action_count, actions.length);
  assert.equal(value.retry_safe, false);
  assert.equal(value.do_not_replay, true);
  return value;
}
const action = (tool, args) => ({ tool, arguments: args });
async function waitFor(selector) {
  return call("android.wait_for_ui_element", { serial, selector, timeout_secs: 15 });
}
async function verifyTwoPointerGesture(orientation, completedGestures) {
  await waitFor({ ...id("pointer_count"), ...text(`Completed two-pointer gestures: ${completedGestures - 1}`) });
  const pad = (await call("android.find_ui_element", { serial, selector: id("touch_pad") })).node.bounds;
  const observed = await call("android.observe", { serial }, { image: true });
  currentGate = `${orientation}_multi_touch_geometry`;
  assert.equal(observed.observation.coordinate_actions_available, true);
  assert.equal(observed.observation.width > observed.observation.height, orientation === "landscape");
  assert.ok(pad.left >= 0 && pad.top >= 0 && pad.right > pad.left && pad.bottom > pad.top &&
    pad.right <= observed.observation.width && pad.bottom <= observed.observation.height,
  "The complete native touch pad must fit the observed image.");
  const x = (fraction) => Math.round(pad.left + (pad.right - pad.left) * fraction);
  const y = (fraction) => Math.round(pad.top + (pad.bottom - pad.top) * fraction);
  const pointers = [
    { x1: x(0.3), y1: y(0.35), x2: x(0.2), y2: y(0.35) },
    { x1: x(0.7), y1: y(0.65), x2: x(0.8), y2: y(0.65) },
  ];
  const touched = await step(`${orientation}-simultaneous-touch`, [action("android.input.multi_touch", {
    pointers, duration_ms: 300,
  })], { observation_id: observed.observation_id });
  assert.match(JSON.stringify(touched.actions), /emulator_grpc/);
  await waitFor({ ...id("pointer_count"), ...text("Max pointers: 2") });
  // The fixture increments only after ACTION_UP with both pointers in their
  // separate expected pad regions. A prior orientation's success cannot satisfy this.
  await waitFor({ ...id("pointer_count"), ...text(`Completed two-pointer gestures: ${completedGestures}`) });
  checks.push({
    gate: `${orientation}_simultaneous_motion_event_pointers`, orientation, ok: true,
    maximum_pointer_count: 2, completed_two_pointer_gestures: completedGestures,
    target_regions_verified: true, target_bounds: pad, pointers,
    image_width: observed.observation.width, image_height: observed.observation.height,
    rotation_quarter_turns: observed.observation.rotation_quarter_turns,
    backend: touched.actions[0].receipt.backend_used,
  });
}

try {
  let occupied = false;
  try { occupied = (await fetch(health, { signal: AbortSignal.timeout(1000) })).ok; } catch { /* no listener */ }
  assert.equal(occupied, false, "Gate must own its MCP endpoint.");
  const providerSha = spawnSync("git", ["-C", repo, "rev-parse", "HEAD"], { encoding: "utf8" });
  assert.equal(providerSha.status, 0);
  assert.equal(providerSha.stdout.trim(), sourceSha, "Fixture and provider must use the same exact checkout.");
  const token = required("ANDROID_COMPUTER_USE_MCP_INTERACTIVE_SESSION_GITHUB_TOKEN");
  server = spawn(binary, [], {
    cwd: repo, stdio: ["ignore", log.fd, log.fd],
    env: {
      ...process.env,
      ANDROID_COMPUTER_USE_MCP_SDK_ROOT: sdk,
      ANDROID_COMPUTER_USE_MCP_ARTIFACT_DIR: artifacts,
      ANDROID_COMPUTER_USE_MCP_BIND_ADDR: endpoint.host,
      ANDROID_COMPUTER_USE_MCP_ENVIRONMENT_ID: `github-${runId}`,
      ANDROID_COMPUTER_USE_MCP_PROVIDER_INSTANCE_ID: "provider-acceptance",
      ANDROID_COMPUTER_USE_MCP_SESSION_ID: `attempt-${process.env.GITHUB_RUN_ATTEMPT || "1"}`,
      ANDROID_COMPUTER_USE_MCP_INTERACTIVE_SESSION_ROOT: hostedSession,
      ANDROID_COMPUTER_USE_MCP_INTERACTIVE_SESSION_GITHUB_REPOSITORY: repository,
      ANDROID_COMPUTER_USE_MCP_INTERACTIVE_SESSION_APP_PACKAGE: packageName,
      ANDROID_COMPUTER_USE_MCP_INTERACTIVE_SESSION_APP_ACTIVITY: activityName,
      ANDROID_COMPUTER_USE_MCP_INTERACTIVE_SESSION_GITHUB_TOKEN: token,
    },
  });
  let spawnError;
  server.on("error", (error) => { spawnError = error; });
  for (let attempt = 0; ; attempt++) {
    assert.ok(!spawnError && server.exitCode === null, "MCP server failed to start; inspect the private local log.");
    let ready = false;
    try { ready = (await fetch(health, { signal: AbortSignal.timeout(1000) })).ok; } catch { /* startup */ }
    if (ready) break;
    assert.ok(attempt < 150, "MCP server readiness timed out.");
    await delay(200);
  }
  client = createMcpStreamableHttpClient({
    endpoint: endpoint.href, clientName: "real-android-provider-acceptance",
    fetchImpl: (url, options) => fetch(url, { ...options, signal: AbortSignal.timeout(420000) }),
  });
  await client.listTools();
  await call("android.wait_for_boot", { serial, timeout_secs: 120 });
  deviceIdentity = deviceBuildIdentity();
  currentGate = "disposable_device_ready";
  deviceReadiness = { before: deviceState() };
  // Window policy's secure field is a conservative startup cache. Require the
  // initialized lock service's explicit current-user NONE credential instead.
  deviceReadiness.credential_check = await pollReadOnlyReadiness(credentialReadiness, (state) => state.state);
  assert.equal(deviceReadiness.credential_check.ready, true, "The initialized lock service must explicitly confirm no current-user credential before input.");
  adbCall(["shell", "svc", "power", "stayon", "true"]);
  adbCall(["shell", "input", "keyevent", "KEYCODE_WAKEUP"]);
  adbCall(["shell", "wm", "dismiss-keyguard"]);
  const { last: readyState, ...uiConfirmation } = await pollReadOnlyReadiness(deviceState, awakeUnlockedReadiness);
  deviceReadiness.after = readyState;
  deviceReadiness.ui_confirmation = uiConfirmation;
  assert.equal(uiConfirmation.ready, true, "The disposable emulator must be explicitly awake and unlocked before UI acceptance.");
  checks.push({ gate: "disposable_device_awake", ok: true, state: deviceReadiness });
  const target = (await call("android.resolve_target", { serial })).resolved_target;
  assert.equal(target.device_serial, serial);
  const expectedBuild = {
    repository, commit_sha: sourceSha, workflow_run_id: runId,
    artifact_name: artifactName, artifact_sha256: `sha256:${archiveDigest}`,
  };
  const build = {
    contract_version: "android-provider-execution/v1",
    workflow_run_id: runId, artifact_name: artifactName, repository,
    install: { launch_after_install: false },
    target: { ...target, expected_build: expectedBuild },
  };
  currentGate = "fresh_fixture_device";
  assert.equal(adbText(["shell", "pm", "path", packageName], true), "", "Fixture must start uninstalled.");
  currentGate = "wrong_provider_identity";
  await assert.rejects(
    client.callTool("android.install_build_from_run", {
      action_id: "provider-acceptance-wrong-target",
      build: { ...build, target: { ...build.target, provider_epoch: "wrong-provider-epoch" } },
    }),
    (error) => /provider_epoch/.test(error.message),
    "Wrong provider epoch must be rejected before installation.",
  );
  checks.push({ gate: "wrong_provider_identity_rejected", ok: true });
  const wrongDigest = await call("android.install_build_from_run", {
    action_id: "provider-acceptance-wrong-digest",
    build: { ...build, target: { ...build.target, expected_build: { ...expectedBuild, artifact_sha256: `sha256:${"0".repeat(64)}` } } },
  }, { ok: false, image: true });
  assert.equal(wrongDigest.install?.error?.kind, "build_provenance_mismatch");
  assert.equal(adbText(["shell", "pm", "path", packageName], true), "");
  checks.push({ gate: "wrong_archive_digest_did_not_install", ok: true });
  const installArgs = { action_id: "provider-acceptance-install", build };
  const installed = await call("android.install_build_from_run", installArgs, { image: true });
  const proof = installed.install?.install_receipt;
  assert.equal(proof?.status, "installed");
  assert.equal(proof.install_performed, true);
  assert.deepEqual(proof.launch, { requested: false, attempted: false, performed: false });
  assert.equal(installed.install.resolved_target.device_serial, serial);
  assert.equal(proof.installed_build.commit_sha, sourceSha);
  assert.equal(proof.installed_build.artifact_sha256, `sha256:${archiveDigest}`);
  assert.equal(adbText(["shell", "pidof", packageName], true), "", "Install-only must not launch the app.");
  const packagePaths = adbText(["shell", "pm", "path", packageName]).split(/\r?\n/);
  assert.equal(packagePaths.length, 1, "Fixture must use one installed base APK.");
  assert.ok(packagePaths[0].startsWith("package:/"));
  const installedDigest = sha256(adbCall(["exec-out", "cat", packagePaths[0].slice(8)]));
  assert.equal(installedDigest, installed.install.manifest.apk_sha256);
  const duplicateInstall = await call("android.install_build_from_run", installArgs, { image: true });
  assert.equal(duplicateInstall.duplicate_action_id, true);
  assert.equal(adbText(["shell", "pidof", packageName], true), "");
  installEvidence = { ...expectedBuild, installed_apk_sha256: installedDigest, install_only: true, duplicate_id_returned_prior_receipt: true };

  await step("launch", [action("android.launch_app", {
    package_name: packageName, activity: activityName,
    wait_for_selector: id("increment"), wait_for_package: packageName,
  })]);
  const increment = [action("android.tap_element", { selector: id("increment"), wait_for_selector: text("Count: 1") })];
  await step("increment-1", increment);
  const duplicateStep = await step("increment-1", increment);
  assert.equal(duplicateStep.duplicate_action_id, true);
  await waitFor(text("Count: 1"));
  const ascii = "Sedna ASCII";
  await step("ascii", [action("android.type_into_element", { selector: id("text_input"), text: ascii })]);
  await waitFor({ ...id("text_input"), ...text(ascii) });
  checks.push({ gate: "ascii_text_verified", ok: true });
  const unicode = "Sedna café Ω %s";
  const typedUnicode = await step("unicode", [action("android.type_into_element", { selector: id("text_input"), text: unicode, timeout_secs: 30 })]);
  assert.equal(typedUnicode.actions[0].receipt.text_backend_used, "emulator_clipboard_adb");
  assert.equal(typedUnicode.actions[0].receipt.clipboard_restore, "restored");
  await waitFor({ ...id("text_input"), ...text(unicode) });
  checks.push({ gate: "unicode_replaced_ascii_via_clipboard", backend: "emulator_clipboard_adb", clipboard_restore: "restored", ok: true });
  await step("hide-keyboard", [action("android.input.keyevent", { keycode: "KEYCODE_BACK" })]);
  await step("dialog", [action("android.tap_element", { selector: id("dialog"), wait_for_selector: text("Acceptance dialog") })]);
  await waitFor(text("Clipboard preserved"));
  checks.push({ gate: "clipboard_restored_after_unicode", ok: true });
  await step("dismiss-dialog", [action("android.input.keyevent", { keycode: "KEYCODE_BACK" })]);
  await waitFor(id("increment"));
  await step("scroll-page", [action("android.tap_element", { selector: id("scroll"), wait_for_selector: text("Scroll acceptance page") })]);
  // Direction describes the finger's motion; swipe up to reveal later rows.
  await step("scroll-to-end", [action("android.scroll_until_visible", { selector: id("back_to_controls"), direction: "up", max_swipes: 8 })]);
  await waitFor(text("Scroll destination reached"));
  await step("scroll-back", [action("android.tap_element", { selector: id("back_to_controls"), wait_for_selector: id("increment") })]);
  recordDeviceGeometry("before_landscape");
  await step("landscape", [action("android.set_orientation", { orientation: "landscape", timeout_secs: 20 })]);
  recordDeviceGeometry("landscape");
  assert.equal((await call("android.get_orientation", { serial })).orientation, "landscape");
  const landscapeTarget = (await call("android.find_ui_element", { serial, selector: id("increment") })).node.bounds;
  const landscape = await call("android.observe", { serial }, { image: true });
  currentGate = "landscape_native_pixel_geometry";
  assert.ok(landscape.observation.width > landscape.observation.height);
  assert.equal(landscape.observation.coordinate_actions_available, true);
  assert.ok(landscapeTarget.left >= 0 && landscapeTarget.top >= 0 &&
    landscapeTarget.right > landscapeTarget.left && landscapeTarget.bottom > landscapeTarget.top &&
    landscapeTarget.right <= landscape.observation.width && landscapeTarget.bottom <= landscape.observation.height,
  "Native landscape target bounds must fit the observed image.");
  const landscapeTap = await step("landscape-increment-2", [action("android.input.tap", {
    x: Math.floor((landscapeTarget.left + landscapeTarget.right) / 2),
    y: Math.floor((landscapeTarget.top + landscapeTarget.bottom) / 2),
    tapped_selector: id("increment"), wait_for_selector: text("Count: 2"),
  })], { observation_id: landscape.observation_id });
  await waitFor(text("Count: 2"));
  checks.push({
    gate: "landscape_native_coordinate_tap", ok: true, expected_counter: 2,
    image_width: landscape.observation.width, image_height: landscape.observation.height,
    rotation_quarter_turns: landscape.observation.rotation_quarter_turns,
    target_bounds: landscapeTarget, backend: landscapeTap.actions[0].receipt.backend_used,
  });
  await verifyTwoPointerGesture("landscape", 1);
  await step("portrait", [action("android.set_orientation", { orientation: "portrait", timeout_secs: 20 })]);
  recordDeviceGeometry("portrait");
  assert.equal((await call("android.get_orientation", { serial })).orientation, "portrait");
  await verifyTwoPointerGesture("portrait", 2);
  for (let count = 3; count <= 12; count++) {
    await step(`increment-${count}`, [action("android.tap_element", {
      selector: id("increment"), wait_for_selector: text(`Count: ${count}`),
    })]);
  }
  await step("terminate", [action("android.terminate_app", { package_name: packageName })]);
  assert.equal(adbText(["shell", "pidof", packageName], true), "");
  await step("relaunch", [action("android.launch_app", {
    package_name: packageName, activity: activityName, wait_for_selector: text("Count: 0"),
  })]);
  const failed = await call("android.step", {
    serial, action_id: "provider-acceptance-partial-failure", actions: [
      action("android.input.keyevent", { keycode: "KEYCODE_BACK" }),
      action("android.tap_element", { selector: id("missing_control"), timeout_secs: 1 }),
      action("android.input.keyevent", { keycode: "KEYCODE_HOME" }),
    ],
  }, { ok: false, image: true });
  assert.equal(failed.executed_action_count, 1);
  assert.equal(failed.attempted_action_count, 2);
  assert.equal(failed.failed_index, 1);
  assert.equal(failed.do_not_replay, true);
  const evidence = {
    schema_version: 1, status: "passed", started_at: new Date(started).toISOString(),
    finished_at: new Date().toISOString(), elapsed_ms: Date.now() - started,
    repository, provider_source_sha: sourceSha, provider_binary_sha256: sha256(await readFile(binary)),
    device_serial: serial, android_build_fingerprint: deviceIdentity.android_build_fingerprint,
    emulator_version: deviceIdentity.emulator_version_line, device_identity: deviceIdentity,
    hosted_build_install_tested: true, actual_simultaneous_touch_tested: true,
    simultaneous_touch_orientations: ["landscape", "portrait"],
    clipboard_restoration_tested: true,
    landscape_coordinate_input_tested: true,
    live_model_judgement: false, multi_device_isolation_tested: false,
    install: installEvidence, device_geometry: geometryEvidence, checks,
  };
  await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, { mode: 0o600 });
  console.log(`Real Android provider acceptance passed (${checks.length} recorded checks).`);
} catch (error) {
  deviceIdentity ||= deviceBuildIdentity();
  recordDeviceGeometry("failure");
  await writeFile(evidencePath, `${JSON.stringify({
    schema_version: 1, status: "failed", failed_gate: currentGate,
    provider_source_sha: sourceSha, elapsed_ms: Date.now() - started,
    hosted_build_install_tested: Boolean(installEvidence),
    clipboard_restoration_tested: checks.some((check) => check.gate === "clipboard_restored_after_unicode" && check.ok),
    landscape_coordinate_input_tested: checks.some((check) => check.gate === "landscape_native_coordinate_tap" && check.ok),
    simultaneous_touch_orientations: checks.filter((check) => check.target_regions_verified && check.ok).map((check) => check.orientation),
    device_readiness: deviceReadiness,
    device_identity: deviceIdentity,
    device_geometry: geometryEvidence,
    install: installEvidence,
    live_model_judgement: false, checks,
  }, null, 2)}\n`, { mode: 0o600 });
  throw error;
} finally {
  if (client) await client.close().catch(() => {});
  if (server && server.exitCode === null) {
    server.kill("SIGTERM");
    for (let attempt = 0; attempt < 50 && server.exitCode === null; attempt++) await delay(100);
    if (server.exitCode === null) server.kill("SIGKILL");
  }
  await log.close();
}
