import assert from "node:assert/strict";
import test from "node:test";
import { assertObservationReceipt, assertStepReceipt, assertTypedPng, callOutput, jsonReceipt } from "./codex_journey_assertions.mjs";

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
