const JSON_RPC_VERSION = "2.0";
const DEFAULT_PROTOCOL_VERSION = "2025-06-18";

function ensureFetch(fetchImpl) {
  if (typeof fetchImpl !== "function") {
    throw new Error("createMcpStreamableHttpClient requires a fetch implementation");
  }
  return fetchImpl;
}

function normalizeHeaders(customHeaders = {}) {
  const headers = new Headers();
  if (customHeaders instanceof Headers) {
    customHeaders.forEach((value, key) => headers.set(key, value));
    return headers;
  }
  for (const [key, value] of Object.entries(customHeaders)) {
    if (value != null && value !== "") {
      headers.set(key, value);
    }
  }
  return headers;
}

async function parseResponsePayload(response) {
  const contentType = response.headers.get("content-type") ?? "";
  const bodyText = await response.text();

  if (contentType.startsWith("application/json")) {
    return JSON.parse(bodyText);
  }

  if (contentType.startsWith("text/event-stream")) {
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
      throw new Error("streamable HTTP response did not contain a JSON event payload");
    }

    return JSON.parse(eventBodies[0]);
  }

  throw new Error(
    `unsupported MCP response content type: ${contentType || "<missing>"} body=${bodyText}`,
  );
}

function normalizeToolResult(result) {
  if (result?.structuredContent != null) {
    return result.structuredContent;
  }
  if (result?.structured_content != null) {
    return result.structured_content;
  }
  if (Array.isArray(result?.content) && result.content.length === 1) {
    const first = result.content[0];
    if (first?.type === "text" && typeof first.text === "string") {
      try {
        return JSON.parse(first.text);
      } catch {
        return first.text;
      }
    }
  }
  return result;
}

function jsonRpcError(method, payload) {
  const error = new Error(
    `${method} failed: ${payload?.error?.message ?? JSON.stringify(payload?.error ?? payload)}`,
  );
  error.payload = payload;
  return error;
}

export function createMcpStreamableHttpClient({
  endpoint,
  fetchImpl = globalThis.fetch,
  clientName = "@sednalabs/android-computer-use-mcp-openai",
  clientVersion = "0.0.0",
  protocolVersion = DEFAULT_PROTOCOL_VERSION,
  capabilities = {},
  authToken = null,
  customHeaders = {},
} = {}) {
  if (!endpoint) {
    throw new Error("createMcpStreamableHttpClient requires an endpoint");
  }

  const fetchFn = ensureFetch(fetchImpl);
  const state = {
    initialized: false,
    initializeResult: null,
    nextId: 1,
    sessionId: null,
  };

  async function postJsonRpc(method, params = undefined) {
    const headers = normalizeHeaders(customHeaders);
    headers.set("accept", "application/json, text/event-stream");
    headers.set("content-type", "application/json");

    if (state.sessionId) {
      headers.set("mcp-session-id", state.sessionId);
    }

    if (authToken) {
      headers.set("authorization", `Bearer ${authToken}`);
    }

    const requestId = state.nextId++;
    const body = {
      jsonrpc: JSON_RPC_VERSION,
      id: requestId,
      method,
    };
    if (params !== undefined) {
      body.params = params;
    }

    const response = await fetchFn(endpoint, {
      method: "POST",
      headers,
      body: JSON.stringify(body),
    });

    const sessionId = response.headers.get("mcp-session-id");
    if (sessionId) {
      state.sessionId = sessionId;
    }

    const payload = await parseResponsePayload(response);
    if (payload?.error) {
      throw jsonRpcError(method, payload);
    }
    if (!response.ok) {
      throw new Error(
        `${method} failed with ${response.status}: ${JSON.stringify(payload)}`,
      );
    }
    return payload;
  }

  return {
    getSessionId() {
      return state.sessionId;
    },

    getClientInfo() {
      return {
        protocolVersion,
        capabilities,
        clientInfo: {
          name: clientName,
          version: clientVersion,
        },
      };
    },

    async initialize() {
      if (state.initialized) {
        return state.initializeResult;
      }

      const payload = await postJsonRpc("initialize", {
        protocolVersion,
        capabilities,
        clientInfo: {
          name: clientName,
          version: clientVersion,
        },
      });

      state.initialized = true;
      state.initializeResult = payload?.result ?? null;
      return state.initializeResult;
    },

    async listTools() {
      await this.initialize();
      const payload = await postJsonRpc("tools/list");
      return payload?.result?.tools ?? [];
    },

    async callTool(name, args = {}) {
      if (!name) {
        throw new Error("callTool requires a tool name");
      }
      await this.initialize();
      const payload = await postJsonRpc("tools/call", {
        name,
        arguments: args,
      });
      return normalizeToolResult(payload?.result);
    },

    async close() {
      if (!state.sessionId) {
        return;
      }

      const headers = normalizeHeaders(customHeaders);
      headers.set("mcp-session-id", state.sessionId);
      if (authToken) {
        headers.set("authorization", `Bearer ${authToken}`);
      }

      try {
        await fetchFn(endpoint, {
          method: "DELETE",
          headers,
        });
      } finally {
        state.initialized = false;
        state.initializeResult = null;
        state.sessionId = null;
      }
    },
  };
}

export { DEFAULT_PROTOCOL_VERSION };
