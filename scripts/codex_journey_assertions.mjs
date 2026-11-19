import assert from "node:assert/strict";
import { createHash } from "node:crypto";

const pngSignature = Buffer.from("89504e470d0a1a0a", "hex");
const crcTable = Uint32Array.from({ length: 256 }, (_, byte) => {
  let value = byte;
  for (let bit = 0; bit < 8; bit++) value = (value >>> 1) ^ ((value & 1) ? 0xedb88320 : 0);
  return value >>> 0;
});
function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) crc = crcTable[(crc ^ byte) & 255] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}

export function assertAndroidToolCatalog(tools, enabledTools) {
  assert.ok(Array.isArray(tools), "Missing model-facing tool catalog.");
  const namespaces = tools.filter((tool) => tool.type === "namespace" && tool.name === "mcp__android");
  assert.equal(namespaces.length, 1, "Expected exactly one Android MCP namespace.");
  const functions = namespaces[0].tools;
  assert.ok(Array.isArray(functions));
  assert.ok(functions.every((tool) => tool.type === "function"), "Android tools must remain function-shaped.");
  const names = functions.map((tool) => tool.name).sort();
  assert.deepEqual(names, enabledTools.map((name) => name.replaceAll(".", "_")).sort(), "The Android MCP catalog differs from the exact enabled-tool allowlist.");
  return names;
}

export function assertAndroidToolDiscovery(body, callId, enabledTools) {
  const outputs = body.input?.filter((item) => item.type === "tool_search_output" && item.call_id === callId);
  assert.equal(outputs?.length, 1, "Missing or duplicated tool-discovery response.");
  assert.equal(outputs[0].status, "completed");
  assert.equal(outputs[0].execution, "client");
  assert.equal(outputs[0].tools?.length, 1, "Exact Android discovery unexpectedly returned another namespace.");
  return assertAndroidToolCatalog(outputs[0].tools, enabledTools);
}

export function callOutput(body, callId) {
  const call = body.input?.find((item) => item.type === "function_call_output" && item.call_id === callId);
  assert.ok(call, `Missing model-facing output for ${callId}.`);
  if (Array.isArray(call.output)) return call.output;
  assert.equal(typeof call.output, "string", `Unexpected model-facing output shape for ${callId}.`);
  return [{ type: "input_text", text: call.output }];
}

export function jsonReceipt(output) {
  const receipts = output
    .filter((item) => item.type === "input_text" && typeof item.text === "string")
    .map((item) => item.text.trim())
    .filter((text) => text.startsWith("{"));
  assert.equal(receipts.length, 1, "Expected one JSON text receipt alongside the image.");
  const receipt = JSON.parse(receipts[0]);
  assert.ok(receipt && typeof receipt === "object" && !Array.isArray(receipt));
  return receipt;
}

// Walk PNG chunks to reject truncated data and placeholders with only a PNG
// signature. The fixture checks wire bytes; it does not judge screen contents.
export function assertTypedPng(output) {
  const images = output.filter((item) => item.type === "input_image");
  const shape = output.map((item) => ({
    type: item.type, detail: item.detail,
    textLength: item.text?.length, imageUrlLength: item.image_url?.length,
  }));
  assert.equal(images.length, 1, `Expected one typed screenshot, received ${JSON.stringify(shape)}.`);
  assert.equal(images[0].detail, "high");
  assert.equal(typeof images[0].image_url, "string");
  assert.match(images[0].image_url, /^data:image\/png;base64,[A-Za-z0-9+/]+={0,2}$/);
  const png = Buffer.from(images[0].image_url.slice("data:image/png;base64,".length), "base64");
  assert.ok(png.subarray(0, 8).equals(pngSignature), "Invalid PNG signature.");
  let offset = 8;
  let width;
  let height;
  let imageData = false;
  let ended = false;
  while (offset < png.length) {
    assert.ok(offset + 12 <= png.length, "Truncated PNG chunk header.");
    const length = png.readUInt32BE(offset);
    assert.ok(offset + 12 + length <= png.length, "Truncated PNG chunk data.");
    const type = png.toString("ascii", offset + 4, offset + 8);
    assert.equal(crc32(png.subarray(offset + 4, offset + 8 + length)), png.readUInt32BE(offset + 8 + length), "PNG chunk checksum mismatch.");
    if (offset === 8) {
      assert.equal(type, "IHDR", "PNG must start with IHDR.");
      assert.equal(length, 13, "Invalid PNG header length.");
      width = png.readUInt32BE(offset + 8);
      height = png.readUInt32BE(offset + 12);
      assert.ok(width > 0 && height > 0, "PNG dimensions must be positive.");
    }
    if (type === "IDAT" && length > 0) imageData = true;
    offset += 12 + length;
    if (type === "IEND") {
      assert.equal(length, 0, "Invalid PNG end chunk.");
      assert.equal(offset, png.length, "Unexpected trailing PNG bytes.");
      ended = true;
      break;
    }
  }
  assert.ok(imageData && ended, "PNG is missing image data or its end marker.");
  return { byte_length: png.length, width, height, sha256: createHash("sha256").update(png).digest("hex") };
}

export function assertBootReadiness(output, serial) {
  const receipt = jsonReceipt(output);
  assert.equal(receipt.ok, true, "Android boot tool did not report readiness.");
  assert.equal(receipt.serial, serial, "Boot receipt identified a different device.");
}

export function assertScreenshotReceipt(output, serial) {
  const receipt = jsonReceipt(output);
  assert.equal(receipt.ok, true);
  assert.equal(receipt.serial, serial);
  assert.equal(typeof receipt.path, "string");
  assert.equal(typeof receipt.backend_used, "string");
  return receipt;
}

export function assertObservationReceipt(output, serial) {
  const receipt = jsonReceipt(output);
  assert.equal(receipt.ok, true, "Observation must not silently report image failure as success.");
  assert.equal(receipt.serial, serial);
  assert.equal(typeof receipt.observation_id, "string", "Missing observation identity.");
  assert.ok(receipt.observation_id.length > 0);
  return receipt;
}

export function assertStepReceipt(output, { serial, actionId, success, count, attempted = count, failedIndex = null, duplicate = false }) {
  const receipt = jsonReceipt(output);
  assert.equal(receipt.serial, serial);
  assert.equal(receipt.action_id, actionId);
  assert.equal(receipt.ok, success, "Step outcome was misreported.");
  assert.equal(receipt.executed_action_count, count, "Step lost its completed-action count.");
  assert.equal(receipt.attempted_action_count, attempted, "Step lost its attempted-action count.");
  assert.equal(receipt.failed_index ?? null, failedIndex, "Step did not stop at the expected action.");
  assert.equal(receipt.duplicate_action_id, duplicate, "Duplicate action identity was not recognised.");
  assert.equal(receipt.do_not_replay, true, "A dispatched mutation must forbid blind replay.");
  assert.equal(receipt.retry_safe, false, "A dispatched mutation must not advertise a safe retry.");
  assert.equal(typeof receipt.outcome, "string");
  assert.ok(Array.isArray(receipt.actions), "Missing individual action receipts.");
  assert.equal(receipt.actions.length, attempted, "Actions continued after failure or receipts were lost.");
  return receipt;
}

export function assertInstallReceipt(output, { serial, actionId, expectedBuild, packageName, launch = true }) {
  const receipt = assertStepReceipt(output, { serial, actionId, success: true, count: 1 });
  const install = receipt.install;
  assert.equal(install?.ok, true, "Hosted install lost its successful phase receipt.");
  assert.equal(install.serial, serial);
  assert.equal(install.contract_version, "android-provider-execution/v1");
  assert.equal(install.installed, true, "The fixture must perform a real installation.");
  assert.equal(install.install_performed, true);
  assert.equal(install.launched, launch);
  assert.equal(install.resolved_target?.device_serial, serial);
  assert.equal(install.manifest?.package_name, packageName);
  assert.match(install.manifest?.apk_sha256 || "", /^[a-f0-9]{64}$/);
  const phase = install.install_receipt;
  assert.equal(phase?.status, "installed");
  assert.deepEqual(phase.requested_build, expectedBuild);
  for (const field of ["repository", "commit_sha", "artifact_name", "artifact_sha256"]) {
    assert.equal(phase.installed_build?.[field], expectedBuild[field], `Installed ${field} differs from the requested build.`);
  }
  assert.equal(String(phase.installed_build?.workflow_run_id), String(expectedBuild.workflow_run_id));
  assert.equal(phase.installed_build?.package_name, packageName);
  assert.match(phase.installed_build?.manifest_sha256 || "", /^sha256:[a-f0-9]{64}$/);
  assert.equal(phase.install_performed, true);
  assert.equal(phase.reused_existing_build, false);
  assert.equal(phase.launch?.requested, launch);
  assert.equal(phase.launch?.attempted, launch);
  assert.equal(phase.launch?.performed, launch);
  return receipt;
}
