#!/usr/bin/env node
// Real-device gate. A missing emulator, hosted artifact or serial-bound gRPC endpoint fails the run.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, open, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { createMcpStreamableHttpClient } from "../adapters/openai/mcp_streamable_http_client.js";
import { assertTypedPng } from "./codex_journey_assertions.mjs";

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
let currentGate = "start";
let server;
let client;
let installEvidence;
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
async function call(name, args, { ok = true, image = false } = {}) {
  currentGate = name;
  const begin = Date.now();
  const result = await client.callTool(name, args);
  const value = receipt(result);
  assert.equal(value.ok, ok, `Unexpected outcome for ${name}.`);
  if (image) assert.equal(Boolean(result.isError), !ok, `${name} MCP error flag must match the receipt.`);
  const imageReceipt = image ? imageEvidence(result) : undefined;
  checks.push({ tool: name, ok, elapsed_ms: Date.now() - begin, ...(imageReceipt ? { image: imageReceipt } : {}) });
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
  const unicode = "Sedna café Ω %s";
  await step("unicode", [action("android.type_into_element", { selector: id("text_input"), text: unicode, timeout_secs: 30 })]);
  await waitFor({ ...id("text_input"), ...text(unicode) });
  await step("hide-keyboard", [action("android.input.keyevent", { keycode: "KEYCODE_BACK" })]);
  await step("dialog", [action("android.tap_element", { selector: id("dialog"), wait_for_selector: text("Acceptance dialog") })]);
  await step("dismiss-dialog", [action("android.input.keyevent", { keycode: "KEYCODE_BACK" })]);
  await waitFor(id("increment"));
  await step("scroll-page", [action("android.tap_element", { selector: id("scroll"), wait_for_selector: text("Scroll acceptance page") })]);
  await step("scroll-to-end", [action("android.scroll_until_visible", { selector: id("back_to_controls"), direction: "down", max_swipes: 8 })]);
  await waitFor(text("Scroll destination reached"));
  await step("scroll-back", [action("android.tap_element", { selector: id("back_to_controls"), wait_for_selector: id("increment") })]);
  await step("landscape", [action("android.set_orientation", { orientation: "landscape", timeout_secs: 20 })]);
  assert.equal((await call("android.get_orientation", { serial })).orientation, "landscape");
  const landscape = await call("android.observe", { serial }, { image: true });
  assert.ok(landscape.observation.width > landscape.observation.height);
  await step("portrait", [action("android.set_orientation", { orientation: "portrait", timeout_secs: 20 })]);
  assert.equal((await call("android.get_orientation", { serial })).orientation, "portrait");
  const pad = (await call("android.find_ui_element", { serial, selector: id("touch_pad") })).node.bounds;
  const observed = await call("android.observe", { serial }, { image: true });
  assert.equal(observed.observation.coordinate_actions_available, true);
  const x = (fraction) => Math.round(pad.left + (pad.right - pad.left) * fraction);
  const y = Math.round((pad.top + pad.bottom) / 2);
  const touched = await step("simultaneous-touch", [action("android.input.multi_touch", {
    pointers: [
      { x1: x(0.3), y1: y, x2: x(0.2), y2: y },
      { x1: x(0.7), y1: y, x2: x(0.8), y2: y },
    ], duration_ms: 300,
  })], { observation_id: observed.observation_id });
  assert.match(JSON.stringify(touched.actions), /emulator_grpc/);
  await waitFor(text("Max pointers: 2"));
  checks.push({ gate: "simultaneous_motion_event_pointers", maximum_pointer_count: 2, ok: true });
  for (let count = 2; count <= 11; count++) {
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
    device_serial: serial, android_build_fingerprint: adbText(["shell", "getprop", "ro.build.fingerprint"]),
    emulator_version: spawnSync(path.join(sdk, "emulator/emulator"), ["-version"], { encoding: "utf8", timeout: 10000 }).stdout.split("\n")[0],
    hosted_build_install_tested: true, actual_simultaneous_touch_tested: true,
    live_model_judgement: false, multi_device_isolation_tested: false,
    install: installEvidence, checks,
  };
  await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, { mode: 0o600 });
  console.log(`Real Android provider acceptance passed (${checks.length} recorded checks).`);
} catch (error) {
  await writeFile(evidencePath, `${JSON.stringify({
    schema_version: 1, status: "failed", failed_gate: currentGate,
    provider_source_sha: sourceSha, elapsed_ms: Date.now() - started,
    hosted_build_install_tested: Boolean(installEvidence),
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
