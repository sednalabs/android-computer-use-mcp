import test from "node:test";
import assert from "node:assert/strict";

import { createMcpStreamableHttpClient } from "./mcp_streamable_http_client.js";

test("streamable MCP client initializes, reuses the session id, and unwraps structured tool results", async () => {
  const requests = [];
  let phase = 0;

  const fetchImpl = async (url, init) => {
    requests.push({
      url,
      method: init.method,
      headers: new Headers(init.headers),
      body: init.body,
    });

    if (phase === 0) {
      phase += 1;
      return new Response(
        JSON.stringify({
          jsonrpc: "2.0",
          id: 1,
          result: { protocolVersion: "2025-06-18" },
        }),
        {
          status: 200,
          headers: {
            "content-type": "application/json",
            "mcp-session-id": "session-123",
          },
        },
      );
    }

    if (phase === 1) {
      phase += 1;
      return new Response(
        JSON.stringify({
          jsonrpc: "2.0",
          id: 2,
          result: {
            structuredContent: {
              ok: true,
              devices: [{ serial: "emulator-5554" }],
            },
          },
        }),
        {
          status: 200,
          headers: {
            "content-type": "application/json",
          },
        },
      );
    }

    return new Response("", { status: 200 });
  };

  const client = createMcpStreamableHttpClient({
    endpoint: "http://127.0.0.1:9526/mcp",
    fetchImpl,
  });

  const initResult = await client.initialize();
  const toolResult = await client.callTool("android.list_devices", {});
  await client.close();

  assert.deepEqual(initResult, { protocolVersion: "2025-06-18" });
  assert.deepEqual(toolResult, {
    ok: true,
    devices: [{ serial: "emulator-5554" }],
  });
  assert.equal(requests.length, 3);
  assert.equal(requests[1].headers.get("mcp-session-id"), "session-123");
  assert.equal(requests[2].method, "DELETE");
  assert.equal(requests[2].headers.get("mcp-session-id"), "session-123");
});
