#!/usr/bin/env node
const endpoint = process.env.MCP_ENDPOINT ?? "http://127.0.0.1:9526/mcp";
const serial = process.env.ANDROID_SERIAL ?? "emulator-5554";
const pngSignature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

let sessionId = null;
let nextId = 1;
let protocolVersion = null;

async function parsePayload(response, method) {
  const contentType = response.headers.get("content-type") ?? "";
  const bodyText = await response.text();
  const normalizedBody = bodyText.replace(/^\uFEFF/, "");
  if (contentType.startsWith("application/json")) {
    return JSON.parse(normalizedBody);
  }
  if (contentType.startsWith("text/event-stream")) {
    const events = normalizedBody
      .split(/\r?\n\r?\n/)
      .map((block) =>
        block
          .split(/\r?\n/)
          .filter((line) => line.startsWith("data:"))
          .map((line) => line.slice("data:".length).trim())
          .join("\n"),
      )
      .filter(Boolean);
    for (const data of events) {
      try {
        const payload = JSON.parse(data);
        if (payload?.result || payload?.error) {
          return payload;
        }
      } catch {
        // Ignore keepalive or non-JSON server events and continue to the response.
      }
    }
    const prefixHex = Buffer.from(normalizedBody).subarray(0, 16).toString("hex");
    throw new Error(
      `${method} returned no JSON-RPC response (HTTP ${response.status}, ${contentType}, ${normalizedBody.length} chars, prefix ${prefixHex || "empty"})`,
    );
  }
  throw new Error(
    `${method} used unsupported response content type (HTTP ${response.status}, ${contentType || "missing"})`,
  );
}

async function rpc(method, params) {
  const headers = new Headers({
    accept: "application/json, text/event-stream",
    "content-type": "application/json",
  });
  if (sessionId) {
    headers.set("mcp-session-id", sessionId);
  }
  if (protocolVersion && method !== "initialize") {
    headers.set("mcp-protocol-version", protocolVersion);
  }

  const response = await fetch(endpoint, {
    method: "POST",
    headers,
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: nextId++,
      method,
      params,
    }),
  });
  const nextSessionId = response.headers.get("mcp-session-id");
  if (nextSessionId) {
    sessionId = nextSessionId;
  }
  const payload = await parsePayload(response, method);
  if (!response.ok || payload?.error) {
    const detail = typeof payload?.error?.message === "string" ? payload.error.message : "no JSON-RPC error detail";
    throw new Error(`${method} failed with HTTP ${response.status}: ${detail.slice(0, 240)}`);
  }
  if (method === "initialize" && typeof payload?.result?.protocolVersion === "string") {
    protocolVersion = payload.result.protocolVersion;
  }
  return payload.result;
}

async function notify(method, params = {}) {
  const headers = new Headers({
    accept: "application/json, text/event-stream",
    "content-type": "application/json",
  });
  if (sessionId) {
    headers.set("mcp-session-id", sessionId);
  }
  if (protocolVersion) {
    headers.set("mcp-protocol-version", protocolVersion);
  }

  const response = await fetch(endpoint, {
    method: "POST",
    headers,
    body: JSON.stringify({
      jsonrpc: "2.0",
      method,
      params,
    }),
  });
  await response.arrayBuffer();
  if (!response.ok) {
    throw new Error(`${method} failed with HTTP ${response.status}`);
  }
}

async function callTool(name, args = {}) {
  return rpc("tools/call", { name, arguments: args });
}

function structuredContent(result) {
  return result?.structuredContent ?? result?.structured_content ?? null;
}

function assertPngImageContent(name, result) {
  const structured = structuredContent(result);
  if (result?.isError || structured?.ok !== true) {
    throw new Error(`${name} did not return successful structured content`);
  }

  const image = (result.content ?? []).find((item) => item?.type === "image");
  if (!image || (image.mimeType ?? image.mime_type) !== "image/png") {
    throw new Error(`${name} did not return a PNG MCP image item`);
  }
  if (typeof image.data !== "string") {
    throw new Error(`${name} returned an image without encoded data`);
  }

  const bytes = Buffer.from(image.data, "base64");
  if (bytes.length < 100 || !bytes.subarray(0, pngSignature.length).equals(pngSignature)) {
    throw new Error(`${name} returned invalid PNG image data`);
  }
  return { tool: name, mime_type: "image/png", image_bytes: bytes.length };
}

async function closeSession() {
  if (!sessionId) {
    return;
  }
  try {
    await fetch(endpoint, { method: "DELETE", headers: { "mcp-session-id": sessionId } });
  } catch {
    // The hosted runner is ephemeral; server shutdown is also handled by the wrapper script.
  }
}

async function main() {
  await rpc("initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "android-image-content-smoke", version: "0.0.0" },
  });
  await notify("notifications/initialized");

  const tools = await rpc("tools/list");
  const toolNames = new Set((tools?.tools ?? []).map((tool) => tool.name));
  for (const required of [
    "android.wait_for_boot",
    "android.capture_screenshot",
    "android.inspect_ui",
    "android.wait_for_stable_ui",
  ]) {
    if (!toolNames.has(required)) {
      throw new Error(`required MCP tool is missing: ${required}`);
    }
  }

  const boot = structuredContent(await callTool("android.wait_for_boot", {
    serial,
    timeout_secs: 120,
  }));
  if (boot?.ok !== true) {
    throw new Error("emulator did not become ready");
  }

  const checked = [];
  checked.push(
    assertPngImageContent(
      "android.capture_screenshot",
      await callTool("android.capture_screenshot", {
        serial,
        filename: "hosted-smoke-capture.png",
      }),
    ),
  );
  checked.push(
    assertPngImageContent(
      "android.inspect_ui",
      await callTool("android.inspect_ui", {
        serial,
        hierarchy_filename: "hosted-smoke-inspect.xml",
        include_screenshot: true,
        screenshot_filename: "hosted-smoke-inspect.png",
      }),
    ),
  );
  checked.push(
    assertPngImageContent(
      "android.wait_for_stable_ui",
      await callTool("android.wait_for_stable_ui", {
        serial,
        timeout_secs: 20,
        poll_interval_ms: 500,
        stable_polls: 1,
        hierarchy_filename: "hosted-smoke-stable.xml",
        include_screenshot: true,
        screenshot_filename: "hosted-smoke-stable.png",
      }),
    ),
  );

  console.log(JSON.stringify({ ok: true, checked }, null, 2));
}

try {
  await main();
} catch (error) {
  console.error(`Android MCP image smoke failed: ${error.message}`);
  process.exitCode = 1;
} finally {
  await closeSession();
}
