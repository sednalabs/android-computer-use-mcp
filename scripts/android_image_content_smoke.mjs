#!/usr/bin/env node
import fs from "node:fs/promises";

const endpoint = process.env.MCP_ENDPOINT ?? "http://127.0.0.1:9526/mcp";
const serial = process.env.ANDROID_SERIAL ?? "emulator-5554";
const summaryPath = process.env.ANDROID_IMAGE_CONTENT_SUMMARY ?? null;
const pngSignature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

let sessionId = null;
let nextId = 1;

function parseEventStream(bodyText) {
  const eventBodies = bodyText
    .split("\n\n")
    .map((chunk) =>
      chunk
        .split("\n")
        .filter((line) => line.startsWith("data:"))
        .map((line) => line.slice("data:".length).trim())
        .join("\n"),
    )
    .filter((chunk) => chunk.length > 0);

  if (eventBodies.length === 0) {
    throw new Error("MCP event-stream response did not include a JSON data event");
  }
  return JSON.parse(eventBodies[0]);
}

async function parsePayload(response) {
  const contentType = response.headers.get("content-type") ?? "";
  const bodyText = await response.text();
  if (contentType.startsWith("application/json")) {
    return JSON.parse(bodyText);
  }
  if (contentType.startsWith("text/event-stream")) {
    return parseEventStream(bodyText);
  }
  throw new Error(
    `unsupported MCP content type ${contentType || "<missing>"} body=${bodyText.slice(0, 500)}`,
  );
}

async function rpc(method, params = undefined) {
  const headers = new Headers({
    accept: "application/json, text/event-stream",
    "content-type": "application/json",
  });
  if (sessionId) {
    headers.set("mcp-session-id", sessionId);
  }

  const body = {
    jsonrpc: "2.0",
    id: nextId++,
    method,
  };
  if (params !== undefined) {
    body.params = params;
  }

  const response = await fetch(endpoint, {
    method: "POST",
    headers,
    body: JSON.stringify(body),
  });
  const nextSessionId = response.headers.get("mcp-session-id");
  if (nextSessionId) {
    sessionId = nextSessionId;
  }

  const payload = await parsePayload(response);
  if (!response.ok || payload?.error) {
    throw new Error(`${method} failed: ${JSON.stringify(payload?.error ?? payload)}`);
  }
  return payload.result;
}

async function closeSession() {
  if (!sessionId) {
    return;
  }
  try {
    await fetch(endpoint, {
      method: "DELETE",
      headers: {
        "mcp-session-id": sessionId,
      },
    });
  } catch (error) {
    console.warn(`failed to close MCP session: ${error.message}`);
  }
}

function structuredContent(result) {
  return result?.structuredContent ?? result?.structured_content ?? null;
}

function assertPngImageContent(toolName, result) {
  const structured = structuredContent(result);
  if (!structured || structured.ok !== true) {
    throw new Error(`${toolName} did not return ok structured content: ${JSON.stringify(structured)}`);
  }

  const imageItems = (result?.content ?? []).filter((item) => item?.type === "image");
  if (imageItems.length === 0) {
    throw new Error(`${toolName} did not return any MCP image content items`);
  }

  const image = imageItems[0];
  const mimeType = image.mimeType ?? image.mime_type;
  if (mimeType !== "image/png") {
    throw new Error(`${toolName} returned image mime type ${mimeType}, expected image/png`);
  }
  if (typeof image.data !== "string" || image.data.length < 32) {
    throw new Error(`${toolName} returned missing or too-small image data`);
  }

  const bytes = Buffer.from(image.data, "base64");
  if (bytes.length < 100) {
    throw new Error(`${toolName} returned only ${bytes.length} decoded image bytes`);
  }
  if (!bytes.subarray(0, pngSignature.length).equals(pngSignature)) {
    throw new Error(`${toolName} image content is not a PNG payload`);
  }

  return {
    tool: toolName,
    content_items: result.content.length,
    image_items: imageItems.length,
    image_mime_type: mimeType,
    image_bytes: bytes.length,
    serial: structured.serial ?? null,
    screenshot_path: structured.path ?? structured.artifacts?.screenshot_path ?? null,
    node_count: structured.node_count ?? null,
  };
}

async function callToolRaw(name, args = {}) {
  return rpc("tools/call", {
    name,
    arguments: args,
  });
}

function sleep(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

async function callToolWithTransientHierarchyRetry(name, args = {}) {
  const attempts = 3;
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      return await callToolRaw(name, args);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (!message.includes("UI hierarchy dump timed out") || attempt === attempts) {
        throw error;
      }
      console.warn(
        `${name} hierarchy capture timed out; retrying ${attempt + 1}/${attempts}`,
      );
      await sleep(1000);
    }
  }
  throw new Error(`${name} hierarchy retry loop did not return a result`);
}

async function main() {
  await rpc("initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: {
      name: "android-image-content-smoke",
      version: "0.0.0",
    },
  });

  const tools = await rpc("tools/list");
  const toolNames = new Set((tools?.tools ?? []).map((tool) => tool.name));
  for (const required of [
    "android.health",
    "android.wait_for_boot",
    "android.capture_screenshot",
    "android.inspect_ui",
    "android.wait_for_stable_ui",
  ]) {
    if (!toolNames.has(required)) {
      throw new Error(`required MCP tool is missing: ${required}`);
    }
  }

  const health = structuredContent(await callToolRaw("android.health", {}));
  console.log(`android.health devices=${JSON.stringify(health?.devices ?? [])}`);

  await callToolRaw("android.wait_for_boot", {
    serial,
    timeout_secs: 120,
  });

  const results = [];
  results.push(
    assertPngImageContent(
      "android.capture_screenshot",
      await callToolRaw("android.capture_screenshot", {
        serial,
        filename: "hosted-smoke-capture.png",
      }),
    ),
  );
  results.push(
    assertPngImageContent(
      "android.inspect_ui",
      await callToolRaw("android.inspect_ui", {
        serial,
        hierarchy_filename: "hosted-smoke-inspect.xml",
        include_screenshot: true,
        screenshot_filename: "hosted-smoke-inspect.png",
      }),
    ),
  );
  results.push(
    assertPngImageContent(
      "android.wait_for_stable_ui",
      await callToolWithTransientHierarchyRetry("android.wait_for_stable_ui", {
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

  const summary = {
    ok: true,
    endpoint,
    serial,
    checked_tools: results,
  };

  console.log(JSON.stringify(summary, null, 2));
  if (summaryPath) {
    await fs.mkdir(new URL(".", `file://${summaryPath}`).pathname, { recursive: true });
    await fs.writeFile(summaryPath, `${JSON.stringify(summary, null, 2)}\n`);
  }
}

try {
  await main();
} finally {
  await closeSession();
}
