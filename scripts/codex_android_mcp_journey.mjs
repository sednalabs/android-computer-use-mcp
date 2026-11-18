#!/usr/bin/env node
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import path from "node:path";
import { zstdDecompressSync } from "node:zlib";
import {
  assertBootReadiness, assertInstallReceipt, assertObservationReceipt, assertScreenshotReceipt,
  assertStepReceipt, assertTypedPng, callOutput, jsonReceipt,
} from "./codex_journey_assertions.mjs";

const codexBinary = process.env.CODEX_BINARY || "codex";
const codexVersion = process.env.CODEX_EXPECTED_VERSION || (process.env.CODEX_BINARY ? null : "0.163.0");
const scenario = process.env.ANDROID_MCP_JOURNEY || "upstream-image";
assert.ok(["upstream-image", "production"].includes(scenario), "Unknown Android MCP journey.");
const protocolMode = process.env.MCP_PROTOCOL_MODE;
if (protocolMode) assert.ok(["legacy", "2026-07-28"].includes(protocolMode), "Unknown MCP protocol preference.");
const serial = process.env.ANDROID_SERIAL || "emulator-5554";
const startedAt = new Date().toISOString();
const checks = [];
let consumerProvenance;
let resolvedInstallTarget;
let expectedBuild;
const fixturePackage = "com.sednalabs.mcp.acceptance";
if (scenario === "production") {
  const artifactDigest = (process.env.ANDROID_ACCEPTANCE_ARTIFACT_SHA256 || "").replace(/^sha256:/, "");
  assert.match(artifactDigest, /^[a-f0-9]{64}$/, "Production acceptance requires the uploaded fixture archive digest.");
  assert.match(process.env.ANDROID_ACCEPTANCE_SOURCE_SHA || "", /^[a-f0-9]{40}$/, "Production acceptance requires the exact fixture source revision.");
  assert.match(process.env.GITHUB_REPOSITORY || "", /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/);
  assert.match(process.env.GITHUB_RUN_ID || "", /^[1-9][0-9]*$/);
  assert.ok(Number.isSafeInteger(Number(process.env.GITHUB_RUN_ID)));
  assert.ok(process.env.ANDROID_ACCEPTANCE_ARTIFACT_NAME, "Production acceptance requires the uploaded fixture artifact name.");
  expectedBuild = {
    repository: process.env.GITHUB_REPOSITORY,
    commit_sha: process.env.ANDROID_ACCEPTANCE_SOURCE_SHA,
    workflow_run_id: Number(process.env.GITHUB_RUN_ID),
    artifact_name: process.env.ANDROID_ACCEPTANCE_ARTIFACT_NAME,
    artifact_sha256: `sha256:${artifactDigest}`,
  };
}

async function verifyConsumer() {
  const version = spawnSync(codexBinary, ["--version"], {
    encoding: "utf8", timeout: 10000,
    env: { PATH: process.env.PATH || "", HOME: process.env.HOME },
  });
  assert.equal(version.status, 0, "The selected Codex CLI did not start.");
  if (codexVersion) {
    assert.ok(version.stdout.trim().split(/\s+/).includes(codexVersion), "Codex CLI version did not match the pinned version.");
  }
  if (process.env.CODEX_BINARY) {
    assert.ok(path.isAbsolute(codexBinary), "CODEX_BINARY must be an absolute executable path.");
    assert.match(process.env.CODEX_SOURCE_SHA || "", /^[a-f0-9]{40}$/, "An exact Codex source SHA is required.");
    assert.ok(process.env.CODEX_BUILD_PROVENANCE, "A source-build provenance receipt is required for a custom Codex binary.");
    consumerProvenance = JSON.parse(await readFile(process.env.CODEX_BUILD_PROVENANCE, "utf8"));
    assert.equal(consumerProvenance.repository, "sednalabs/codex");
    assert.equal(consumerProvenance.source_sha, process.env.CODEX_SOURCE_SHA);
    const binaryDigest = createHash("sha256").update(await readFile(codexBinary)).digest("hex");
    assert.equal(consumerProvenance.binary_sha256, binaryDigest, "Codex binary does not match its build receipt.");
  } else {
    assert.equal(scenario, "upstream-image", "The production journey requires an exact Sedna Codex source build.");
    consumerProvenance = { kind: "version-pinned-cli", version_pin: codexVersion, source_revision_verified: false };
  }
  consumerProvenance = { ...consumerProvenance, version_output: version.stdout.trim() };
}
const fakeKey = "android-mcp-disposable-ci-key";
const mcpEndpoint = process.env.MCP_ENDPOINT ?? "http://127.0.0.1:9526/mcp";
const endpointUrl = new URL(mcpEndpoint);
assert.ok(endpointUrl.protocol === "http:" && ["127.0.0.1", "localhost", "[::1]"].includes(endpointUrl.hostname), "The disposable journey must use a loopback MCP endpoint.");
assert.ok(!endpointUrl.username && !endpointUrl.password, "MCP endpoint must not contain credentials.");
// Codex helper aliases must live outside the global /tmp directory when the
// host provides a private job directory. The consumer home remains isolated.
const tmpRoot = await mkdtemp(path.join(process.env.RUNNER_TEMP || tmpdir(), "android-mcp-codex-consumer-"));
const codexHome = path.join(tmpRoot, "codex-home");
const codexCwd = path.join(tmpRoot, "workspace");
await mkdir(codexHome);
await mkdir(codexCwd);

const sse = (events) => events.map((event) => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join("");
const responseCreated = (id) => ({ type: "response.created", response: { id } });
const responseCompleted = (id) => ({
  type: "response.completed",
  response: {
    id,
    usage: {
      input_tokens: 0,
      input_tokens_details: null,
      output_tokens: 0,
      output_tokens_details: null,
      total_tokens: 0,
    },
  },
});
const toolCall = (responseId, callId, name, args) => sse([
  responseCreated(responseId),
  {
    type: "response.output_item.done",
    item: {
      type: "function_call",
      call_id: callId,
      namespace: "mcp__android",
      name,
      arguments: JSON.stringify(args),
    },
  },
  responseCompleted(responseId),
]);
const finalMessage = (responseId) => sse([
  responseCreated(responseId),
  {
    type: "response.output_item.done",
    item: {
      type: "message",
      role: "assistant",
      id: `${responseId}-message`,
      content: [{ type: "output_text", text: "Android MCP screenshot was received as typed image input." }],
    },
  },
  responseCompleted(responseId),
]);

const successActionId = `codex-consumer-${protocolMode || "default"}-success`;
const failureActionId = `codex-consumer-${protocolMode || "default"}-partial-failure`;
const installActionId = `codex-consumer-${protocolMode || "default"}-install`;
const successArgs = {
  serial, action_id: successActionId,
  actions: [{ tool: "android.input.keyevent", arguments: { keycode: "KEYCODE_HOME" } }],
};
const steps = [{
  name: "android_wait_for_boot", args: { serial, timeout_secs: 120 },
  check(output) { assertBootReadiness(output, serial); return { gate: "boot_readiness" }; },
}];
if (scenario === "upstream-image") {
  steps.push({
    name: "android_capture_screenshot", args: { serial, filename: "codex-consumer-screenshot.png" },
    check(output) { assertScreenshotReceipt(output, serial); return { gate: "typed_screenshot", image: assertTypedPng(output) }; },
  });
} else {
  steps.push({
    name: "android_resolve_target", args: { serial },
    check(output) {
      const receipt = jsonReceipt(output);
      assert.equal(receipt.ok, true);
      assert.equal(receipt.contract_version, "android-provider-execution/v1");
      const target = receipt.resolved_target;
      assert.equal(target?.device_serial, serial);
      resolvedInstallTarget = {};
      for (const key of ["environment_id", "provider_instance_id", "provider_epoch", "session_id", "device_serial"]) {
        assert.ok(typeof target[key] === "string" && target[key].length > 0, `Missing exact target field ${key}.`);
        resolvedInstallTarget[key] = target[key];
      }
      return { gate: "exact_install_target", target: resolvedInstallTarget };
    },
  }, {
    name: "android_install_build_from_run", args() {
      assert.ok(resolvedInstallTarget, "Resolve the provider before constructing its install request.");
      return {
        action_id: installActionId,
        build: {
          contract_version: "android-provider-execution/v1",
          workflow_run_id: expectedBuild.workflow_run_id,
          artifact_name: expectedBuild.artifact_name,
          repository: expectedBuild.repository,
          install: { launch_after_install: true },
          target: { ...resolvedInstallTarget, expected_build: expectedBuild },
        },
      };
    },
    check(output) {
      const receipt = assertInstallReceipt(output, { serial, actionId: installActionId, expectedBuild, packageName: fixturePackage });
      return {
        gate: "hosted_install_receipt_and_image", image: assertTypedPng(output),
        installed_build: receipt.install.install_receipt.installed_build,
        apk_sha256: receipt.install.manifest.apk_sha256,
      };
    },
  }, {
    name: "android_observe", args: { serial },
    check(output) {
      const receipt = assertObservationReceipt(output, serial);
      return { gate: "observation", observation_id: receipt.observation_id, image: assertTypedPng(output) };
    },
  }, {
    name: "android_step", args: successArgs,
    check(output) {
      assertStepReceipt(output, { serial, actionId: successActionId, success: true, count: 1 });
      return { gate: "successful_step", image: assertTypedPng(output) };
    },
  }, {
    name: "android_step", args: successArgs,
    check(output) {
      assertStepReceipt(output, { serial, actionId: successActionId, success: true, count: 1, duplicate: true });
      return { gate: "duplicate_action_id", image: assertTypedPng(output) };
    },
  }, {
    name: "android_step", args: {
      serial, action_id: failureActionId,
      actions: [
        { tool: "android.input.keyevent", arguments: { keycode: "KEYCODE_BACK" } },
        { tool: "android.tap_element", arguments: {
          selector: { resource_id: "android.mcp.fixture:id/nonexistent_control" }, timeout_secs: 1,
        } },
        { tool: "android.input.keyevent", arguments: { keycode: "KEYCODE_HOME" } },
      ],
    },
    check(output) {
      assertStepReceipt(output, {
        serial, actionId: failureActionId, success: false, count: 1, attempted: 2, failedIndex: 1,
      });
      return { gate: "partial_failure_receipt_and_image", image: assertTypedPng(output) };
    },
  });
}
const enabledTools = scenario === "upstream-image"
  ? ["android.wait_for_boot", "android.capture_screenshot"]
  : ["android.wait_for_boot", "android.resolve_target", "android.install_build_from_run", "android.observe", "android.step"];

let api;
let codex;
let apiFailure;
let requestCount = 0;
let stderr = "";

try {
  await verifyConsumer();

  api = createServer(async (request, response) => {
    try {
      if (request.method === "GET" && request.url?.startsWith("/v1/models")) {
        response.writeHead(200, { "content-type": "application/json" });
        response.end(JSON.stringify({ object: "list", data: [{ id: "gpt-5.2", object: "model", created: 0, owned_by: "fixture" }] }));
        return;
      }

      assert.equal(request.method, "POST");
      assert.equal(request.url, "/v1/responses");
      if (request.headers.authorization !== `Bearer ${fakeKey}`) {
        throw new Error("Only the disposable fixture key may reach the local mock provider.");
      }
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      let bytes = Buffer.concat(chunks);
      const encoding = String(request.headers["content-encoding"] || "").toLowerCase();
      if (encoding === "zstd") bytes = zstdDecompressSync(bytes);
      else if (encoding && encoding !== "identity") throw new Error(`Unexpected request encoding: ${encoding}`);
      const body = JSON.parse(bytes.toString("utf8"));
      requestCount++;

      if (requestCount === 1) {
        const android = body.tools?.find((tool) => tool.type === "namespace" && tool.name === "mcp__android");
        assert.deepEqual(android?.tools?.map((tool) => tool.name).sort(), enabledTools.map((name) => name.replaceAll(".", "_")).sort());
      } else {
        const previous = steps[requestCount - 2];
        assert.ok(previous, `Unexpected extra Responses API request ${requestCount}.`);
        checks.push(previous.check(callOutput(body, `android-mcp-call-${requestCount - 1}`)));
      }
      const next = steps[requestCount - 1];
      const stream = next
        ? toolCall(`android-mcp-response-${requestCount}`, `android-mcp-call-${requestCount}`, next.name, typeof next.args === "function" ? next.args() : next.args)
        : finalMessage(`android-mcp-response-${requestCount}`);

      response.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache", connection: "keep-alive" });
      response.end(stream);
    } catch (error) {
      apiFailure ??= new Error(`Mock-provider assertion failed: ${error.message}`);
      response.writeHead(500, { "content-type": "text/plain" });
      response.end("Disposable Codex consumer fixture assertion failed.");
    }
  });
  await new Promise((resolve, reject) => {
    api.once("error", reject);
    api.listen(0, "127.0.0.1", resolve);
  });

  const config = [
    'model = "gpt-5.2"',
    'model_provider = "android_fixture"',
    "model_context_window = 128000",
    'approval_policy = "on-request"',
    'sandbox_mode = "read-only"',
    "",
    "[model_providers.android_fixture]",
    'name = "Disposable Android MCP test provider"',
    `base_url = ${JSON.stringify(`http://127.0.0.1:${api.address().port}/v1`)}`,
    'env_key = "ANDROID_MCP_TEST_API_KEY"',
    'wire_api = "responses"',
    "requires_openai_auth = false",
    "request_max_retries = 0",
    "stream_max_retries = 0",
    "",
    "[mcp_servers.android]",
    `url = ${JSON.stringify(mcpEndpoint)}`,
    "required = true",
    "startup_timeout_sec = 20",
    "tool_timeout_sec = 420",
    `enabled_tools = ${JSON.stringify(enabledTools)}`,
    ...(protocolMode ? [`protocol_mode = ${JSON.stringify(protocolMode)}`] : []),
    'default_tools_approval_mode = "approve"',
    "",
  ].join("\n");
  await writeFile(path.join(codexHome, "config.toml"), config, { mode: 0o600 });

  codex = spawn(codexBinary, [
    "exec",
    "--json",
    "--ephemeral",
    "--skip-git-repo-check",
    "--model",
    "gpt-5.2",
    "--sandbox",
    "read-only",
    "--color",
    "never",
    "--cd",
    codexCwd,
    "Use only the configured Android MCP server to execute the disposable emulator fixture. Do not use shell tools or other servers.",
  ], {
    cwd: codexCwd,
    env: {
      PATH: process.env.PATH || "",
      HOME: process.env.HOME,
      CODEX_HOME: codexHome,
      ANDROID_MCP_TEST_API_KEY: fakeKey,
    },
    stdio: ["ignore", "ignore", "pipe"],
  });
  codex.stderr.setEncoding("utf8").on("data", (chunk) => { stderr = (stderr + chunk).slice(-3000); });
  const exit = await new Promise((resolve, reject) => {
    let hardKill;
    const softKill = setTimeout(() => {
      codex.kill("SIGTERM");
      hardKill = setTimeout(() => codex.kill("SIGKILL"), 10000);
    }, scenario === "production" ? 720000 : 360000);
    codex.once("error", reject);
    codex.once("close", (code, signal) => resolve({ code, signal }));
    codex.once("close", () => { clearTimeout(softKill); clearTimeout(hardKill); });
  });
  assert.equal(apiFailure, undefined, apiFailure?.message);
  assert.equal(exit.code, 0, `Codex CLI exited ${exit.code ?? exit.signal}: ${stderr.replaceAll(fakeKey, "[redacted]").slice(-1200)}`);
  assert.equal(requestCount, steps.length + 1, "Every tool result must return to the model before the fixture completes.");
  const evidence = {
    schema_version: 1, status: "passed", scenario, started_at: startedAt,
    completed_at: new Date().toISOString(), consumer: consumerProvenance,
    mcp_source_sha: process.env.MCP_SOURCE_SHA || null,
    mcp_binary_sha256: process.env.MCP_BINARY_SHA256 || null,
    requested_protocol_mode: protocolMode || "client_default",
    negotiated_protocol_version: null,
    serial, android_build_fingerprint: process.env.ANDROID_BUILD_FINGERPRINT || null,
    emulator_version: process.env.ANDROID_EMULATOR_VERSION || null,
    scripted_model_fixture: true, live_model_judgement: false,
    hosted_build_install_tested: checks.some((check) => check.gate === "hosted_install_receipt_and_image"), checks,
  };
  if (process.env.ANDROID_MCP_JOURNEY_EVIDENCE) {
    await writeFile(process.env.ANDROID_MCP_JOURNEY_EVIDENCE, `${JSON.stringify(evidence, null, 2)}\n`, { mode: 0o600 });
  }
  process.stdout.write(`Codex generic MCP ${scenario} journey passed: ${checks.length} wire assertions, typed PNGs preserved.\n`);
} finally {
  if (codex && codex.exitCode === null && codex.signalCode === null) {
    codex.kill("SIGTERM");
    await new Promise((resolve) => codex.once("close", resolve));
  }
  if (api?.listening) {
    api.closeAllConnections?.();
    await new Promise((resolve) => api.close(resolve));
  }
  await rm(tmpRoot, { recursive: true, force: true });
}
