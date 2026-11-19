import assert from "node:assert/strict";
import test from "node:test";
import { assertAndroidToolCatalog, assertAndroidToolDiscovery, assertInstallReceipt, assertObservationReceipt, assertStepReceipt, assertTypedPng, callOutput, jsonReceipt } from "./codex_journey_assertions.mjs";

const png = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=", "base64");
const image = { type: "input_image", detail: "high", image_url: `data:image/png;base64,${png.toString("base64")}` };
const text = (receipt) => ({ type: "input_text", text: JSON.stringify(receipt) });
const step = {
  ok: false, serial: "emulator-5554", action_id: "partial-batch", outcome: "postcondition_failed",
  duplicate_action_id: false, executed_action_count: 1, attempted_action_count: 2, failed_index: 1,
  do_not_replay: true, retry_safe: false,
  actions: [{ index: 0, outcome: "dispatched" }, { index: 1, outcome: "postcondition_failed" }],
};
const expected = { serial: step.serial, actionId: step.action_id, success: false, count: 1, attempted: 2, failedIndex: 1 };

test("direct and deferred catalogs preserve the exact Android allowlist", () => {
  const enabled = ["android.observe", "android.step"];
  const catalog = [{ type: "namespace", name: "mcp__android", tools: [
    { type: "function", name: "android_step" }, { type: "function", name: "android_observe" },
  ] }];
  const output = { type: "tool_search_output", call_id: "discover-1", status: "completed", execution: "client", tools: catalog };
  assert.deepEqual(assertAndroidToolCatalog(catalog, enabled), ["android_observe", "android_step"]);
  assert.deepEqual(assertAndroidToolDiscovery({ input: [output] }, "discover-1", enabled), ["android_observe", "android_step"]);
  assert.throws(() => assertAndroidToolDiscovery({ input: [output] }, "another-call", enabled));
  assert.throws(() => assertAndroidToolDiscovery({ input: [output, output] }, "discover-1", enabled));
  for (const tools of [
    [],
    [{ ...catalog[0], name: "mcp__another_device" }],
    [{ ...catalog[0], tools: catalog[0].tools.slice(0, 1) }],
    [{ ...catalog[0], tools: [...catalog[0].tools, { type: "function", name: "android_uninstall_app" }] }],
  ]) {
    assert.throws(() => assertAndroidToolCatalog(tools, enabled));
    assert.throws(() => assertAndroidToolDiscovery({ input: [{ ...output, tools }] }, "discover-1", enabled));
  }
});

test("model-facing typed PNG bytes retain dimensions and digest", () => {
  const result = assertTypedPng([text({ ok: true }), image]);
  assert.equal(result.byte_length, png.length);
  assert.equal(result.width, 1);
  assert.equal(result.height, 1);
  assert.match(result.sha256, /^[a-f0-9]{64}$/);
});

test("wire assertion rejects text-only, duplicate, and truncated screenshots", () => {
  assert.throws(() => assertTypedPng([text({ type: "image", data: png.toString("base64") })]), /typed screenshot/);
  assert.throws(() => assertTypedPng([image, image]), /typed screenshot/);
  const truncated = { ...image, image_url: `data:image/png;base64,${png.subarray(0, -4).toString("base64")}` };
  assert.throws(() => assertTypedPng([truncated]), /Truncated/);
  const corrupted = Buffer.from(png);
  corrupted[45] ^= 1;
  assert.throws(() => assertTypedPng([{ ...image, image_url: `data:image/png;base64,${corrupted.toString("base64")}` }]), /checksum/);
});

test("partial batch failure retains completed actions, stop index and no-replay flags", () => {
  assert.deepEqual(assertStepReceipt([text(step), image], expected), step);
  for (const bad of [
    { ok: true }, { serial: "emulator-5556" }, { executed_action_count: 3 },
    { attempted_action_count: 3 }, { failed_index: 2 }, { retry_safe: true },
    { do_not_replay: false }, { actions: [{}, {}, {}] },
  ]) assert.throws(() => assertStepReceipt([text({ ...step, ...bad }), image], expected));
});

test("duplicate action identity cannot be silently treated as new execution", () => {
  assert.throws(() => assertStepReceipt([text(step)], { ...expected, duplicate: true }));
  assertStepReceipt([text({ ...step, duplicate_action_id: true })], { ...expected, duplicate: true });
});

test("observations must carry a usable identity and the exact device", () => {
  assertObservationReceipt([text({ ok: true, serial: step.serial, observation_id: "frame-1" })], step.serial);
  assert.throws(() => assertObservationReceipt([text({ ok: true, serial: step.serial })], step.serial));
  assert.throws(() => assertObservationReceipt([text({ ok: false, serial: step.serial, observation_id: "frame-1" })], step.serial));
});

test("output extraction retains call identity and rejects multiple receipts", () => {
  const body = { input: [{ type: "function_call_output", call_id: "call-a", output: [text(step), image] }] };
  assert.deepEqual(callOutput(body, "call-a"), [text(step), image]);
  assert.throws(() => callOutput(body, "call-b"), /Missing model-facing output/);
  assert.throws(() => jsonReceipt([text(step), text(step)]), /one JSON text receipt/);
});

test("text receipts accept the exact Codex timing envelope without extracting arbitrary JSON", () => {
  const wrapped = { type: "input_text", text: `Wall time: 1.2500 seconds\nOutput:\n${JSON.stringify(step)}` };
  assert.deepEqual(jsonReceipt([wrapped]), step);
  assert.throws(() => jsonReceipt([text(step), wrapped]), /one JSON text receipt/);
  for (const prefix of ["Error: ", "Untrusted prose\n", "Wall time: unknown seconds\nOutput:\n"]) {
    assert.throws(() => jsonReceipt([{ type: "input_text", text: `${prefix}${JSON.stringify(step)}` }]), /one JSON text receipt/);
  }
  assert.throws(() => jsonReceipt([{ type: "input_text", text: "Wall time: 1.2500 seconds\nOutput:\n{\"ok\":" }]));
});

test("hosted install evidence rejects wrong build identity and missing installation effects", () => {
  const expectedBuild = {
    repository: "fixture/app", commit_sha: "a".repeat(40), workflow_run_id: 42,
    artifact_name: "fixture-apk", artifact_sha256: `sha256:${"b".repeat(64)}`,
  };
  const installedBuild = { ...expectedBuild, workflow_run_id: "42", package_name: "fixture.app", manifest_sha256: `sha256:${"c".repeat(64)}` };
  const receipt = {
    ...step, ok: true, action_id: "install-a", outcome: "verified", attempted_action_count: 1,
    failed_index: null, actions: [{ index: 0, outcome: "verified" }],
    install: {
      ok: true, serial: step.serial, contract_version: "android-provider-execution/v1",
      installed: true, install_performed: true, launched: true,
      resolved_target: { device_serial: step.serial },
      manifest: { package_name: "fixture.app", apk_sha256: "d".repeat(64) },
      install_receipt: {
        status: "installed", requested_build: expectedBuild, installed_build: installedBuild,
        install_performed: true, reused_existing_build: false,
        launch: { requested: true, attempted: true, performed: true },
      },
    },
  };
  const expected = { serial: step.serial, actionId: "install-a", expectedBuild, packageName: "fixture.app" };
  assertInstallReceipt([text(receipt), image], expected);
  for (const modify of [
    (value) => { value.install.installed = false; },
    (value) => { value.install.install_performed = false; },
    (value) => { value.install.install_receipt.installed_build.commit_sha = "e".repeat(40); },
    (value) => { value.install.install_receipt.installed_build.artifact_sha256 = `sha256:${"e".repeat(64)}`; },
    (value) => { value.install.install_receipt.installed_build.workflow_run_id = "43"; },
    (value) => { value.install.install_receipt.launch.performed = false; },
    (value) => { value.install.resolved_target.device_serial = "emulator-5556"; },
  ]) {
    const changed = structuredClone(receipt);
    modify(changed);
    assert.throws(() => assertInstallReceipt([text(changed), image], expected));
  }
});
