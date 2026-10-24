#!/usr/bin/env node
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import path from "node:path";
import { zstdDecompressSync } from "node:zlib";

const codexVersion = "0.163.0";
const fakeKey = "android-mcp-disposable-ci-key";
const mcpEndpoint = process.env.MCP_ENDPOINT ?? "http://127.0.0.1:9526/mcp";
const tmpRoot = await mkdtemp(path.join(tmpdir(), "android-mcp-codex-consumer-"));
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

function assertTypedPng(output) {
  const images = output.filter((item) => item.type === "input_image");
  assert.equal(images.length, 1, "Expected one typed screenshot in the model request.");
  assert.equal(images[0].detail, "high");
  assert.ok(/^data:image\/png;base64,/.test(images[0].image_url), "Screenshot was not forwarded as a PNG data URL.");
  const png = Buffer.from(images[0].image_url.slice("data:image/png;base64,".length), "base64");
  assert.equal(png.subarray(0, 8).toString("hex"), "89504e470d0a1a0a");
  assert.ok(png.length > 100, "Screenshot did not contain real image bytes.");
  return png.length;
}

function callOutput(body, callId) {
  const call = body.input?.find((item) => item.type === "function_call_output" && item.call_id === callId);
  assert.ok(call, `Missing model-facing output for ${callId}.`);
  if (Array.isArray(call.output)) return call.output;
  assert.equal(typeof call.output, "string", `Unexpected model-facing output shape for ${callId}.`);
  return [{ type: "input_text", text: call.output }];
}

function assertBootReadiness(output) {
  const text = output.filter((item) => item.type === "input_text").map((item) => item.text).join("\n");
  assert.match(text, /"ok"\s*:\s*true/, "Android boot tool did not report readiness.");
  assert.match(text, /"serial"\s*:\s*"emulator-5554"/, "Android boot result did not identify the target emulator.");
}

let api;
let codex;
let apiFailure;
let requestCount = 0;
let stderr = "";

try {
  const version = spawnSync("codex", ["--version"], {
    encoding: "utf8",
    env: { PATH: process.env.PATH || "", HOME: process.env.HOME },
  });
  assert.equal(version.status, 0, "The pinned Codex CLI did not start.");
  assert.match(version.stdout, new RegExp(`\\b${codexVersion.replaceAll(".", "\\.")}\\b`));

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

      let stream;
      if (requestCount === 1) {
        const android = body.tools?.find((tool) => tool.type === "namespace" && tool.name === "mcp__android");
        assert.deepEqual(android?.tools?.map((tool) => tool.name).sort(), [
          "android_capture_screenshot",
          "android_wait_for_boot",
        ]);
        stream = toolCall("android-mcp-response-1", "android-mcp-call-1", "android_wait_for_boot", {
          serial: "emulator-5554",
          timeout_secs: 120,
        });
      } else if (requestCount === 2) {
        const output = callOutput(body, "android-mcp-call-1");
        assertBootReadiness(output);
        stream = toolCall("android-mcp-response-2", "android-mcp-call-2", "android_capture_screenshot", {
          serial: "emulator-5554",
          filename: "codex-consumer-screenshot.png",
        });
      } else if (requestCount === 3) {
        const output = callOutput(body, "android-mcp-call-2");
        const imageBytes = assertTypedPng(output);
        stream = finalMessage("android-mcp-response-3");
        response.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache", connection: "keep-alive" });
        response.end(stream);
        process.stdout.write(`Codex ${codexVersion} generic HTTP MCP journey passed: Android tools discovered, device readiness checked, and screenshot forwarded as typed PNG (${imageBytes} bytes).\n`);
        return;
      } else {
        throw new Error(`Unexpected extra Responses API request ${requestCount}.`);
      }

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
    "tool_timeout_sec = 60",
    'enabled_tools = ["android.wait_for_boot", "android.capture_screenshot"]',
    'default_tools_approval_mode = "approve"',
    "",
  ].join("\n");
  await writeFile(path.join(codexHome, "config.toml"), config, { mode: 0o600 });

  codex = spawn("codex", [
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
    "Use only the configured Android MCP server. First verify the emulator is ready, then capture one screenshot. Do not use shell tools or other servers.",
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
    }, 150000);
    codex.once("error", reject);
    codex.once("close", (code, signal) => resolve({ code, signal }));
    codex.once("close", () => { clearTimeout(softKill); clearTimeout(hardKill); });
  });
  assert.equal(apiFailure, undefined, apiFailure?.message);
  assert.equal(exit.code, 0, `Codex CLI exited ${exit.code ?? exit.signal}: ${stderr.replaceAll(fakeKey, "[redacted]").slice(-1200)}`);
  assert.equal(requestCount, 3, "Expected readiness and screenshot calls followed by a model request containing its typed image.");
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
