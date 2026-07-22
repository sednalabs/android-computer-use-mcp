#!/usr/bin/env node

import path from "node:path";
import { promises as fs } from "node:fs";

import { createAndroidEmulatorRuntime } from "../../../docs/examples/android_emulator_runtime_client.js";
import { createMcpStreamableHttpClient } from "../../openai/mcp_streamable_http_client.js";
import {
  loadCodexAdapterConfig,
  resolveMcpHeaders,
  resolveMcpUrl,
} from "../cli_common.js";
import {
  contextFromObservation,
  createCodexThreadItemsAdapter,
} from "../index.js";

function usage() {
  console.log(`Usage: codex-android-observe.mjs [options]

Captures a fresh Android observation from a live android-computer-use-mcp endpoint
and emits Codex-ready raw Responses items without requiring OPENAI_API_KEY.

Options:
  --config PATH               Shared hosted-session config JSON
  --mcp-url URL               MCP streamable HTTP endpoint
  --mcp-header NAME=VALUE     Extra MCP request header (repeatable)
  --serial SERIAL             Android serial to target
  --caption TEXT              Summary caption for the emitted observation
  --role ROLE                 Message role for Codex thread items (default: assistant)
  --mode MODE                 message or thread-inject (default: message)
  --thread-id ID              Required for thread-inject mode
  --instant                   Capture immediate UI state instead of waiting for stability
  --output-json PATH          Write the raw JSON payload to a file
  -h, --help                  Show this help
`);
}

function parseArgs(argv) {
  const parsed = {
    mcpHeaders: [],
    mode: "message",
    role: "assistant",
    stable: true,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    switch (arg) {
      case "--config":
        parsed.configPath = argv[++index];
        break;
      case "--mcp-url":
        parsed.mcpUrl = argv[++index];
        break;
      case "--mcp-header":
        parsed.mcpHeaders.push(argv[++index]);
        break;
      case "--serial":
        parsed.serial = argv[++index];
        break;
      case "--caption":
        parsed.caption = argv[++index];
        break;
      case "--role":
        parsed.role = argv[++index];
        break;
      case "--mode":
        parsed.mode = argv[++index];
        break;
      case "--thread-id":
        parsed.threadId = argv[++index];
        break;
      case "--instant":
        parsed.stable = false;
        break;
      case "--output-json":
        parsed.outputJson = argv[++index];
        break;
      case "-h":
      case "--help":
        parsed.help = true;
        break;
      default:
        throw new Error(`Unknown argument: ${arg}`);
    }
  }

  return parsed;
}

async function writeJson(filePath, value) {
  await fs.mkdir(path.dirname(filePath), { recursive: true });
  await fs.writeFile(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    usage();
    return;
  }

  const config = await loadCodexAdapterConfig({
    configPath: args.configPath,
  });
  const endpoint = resolveMcpUrl({
    explicitUrl: args.mcpUrl,
    config,
  });
  if (!endpoint) {
    throw new Error(
      "codex-android-observe requires --mcp-url, a config with mcp_url, or CODEX_ANDROID_MCP_URL/CODEX_ANDROID_MCP_HOSTNAME",
    );
  }

  const customHeaders = resolveMcpHeaders({
    headerArgs: args.mcpHeaders,
  });
  const defaultSerial = args.serial ?? config.default_serial ?? null;
  const caption = args.caption ?? "Hosted Android observation";
  const mode = args.mode ?? "message";
  if (!["message", "thread-inject"].includes(mode)) {
    throw new Error(`Unsupported --mode value: ${mode}`);
  }
  if (mode === "thread-inject" && !args.threadId) {
    throw new Error("--thread-id is required when --mode thread-inject is used");
  }

  const mcpClient = createMcpStreamableHttpClient({
    endpoint,
    customHeaders,
  });

  try {
    await mcpClient.initialize();

    const runtime = createAndroidEmulatorRuntime({
      callMcp: (toolName, toolArgs) => mcpClient.callTool(toolName, toolArgs),
      displayScreenshot: async () => {},
      defaultSerial,
    });

    if (!runtime.getState().currentSerial) {
      const devices = await runtime.listDevices();
      const serial = devices?.devices?.[0]?.serial ?? null;
      if (!serial) {
        throw new Error("No Android device serial is available for the Codex bridge");
      }
      await runtime.setSerial(serial);
    }

    const observation = args.stable === false
      ? await runtime.inspectUi({
          hierarchyFilename: "codex-android-observe.xml",
          screenshotFilename: "codex-android-observe.png",
        })
      : await runtime.waitForStableUi({
          hierarchyFilename: "codex-android-observe.xml",
          screenshotFilename: "codex-android-observe.png",
        });

    const adapter = createCodexThreadItemsAdapter({
      readFile: fs.readFile,
    });

    const context = contextFromObservation(observation);
    const payload =
      mode === "thread-inject"
        ? await adapter.threadInjectItemsFromVisualContext(args.threadId, context, {
            role: args.role,
            caption,
          })
        : await adapter.visualContextMessage(context, {
            role: args.role,
            caption,
          });

    if (args.outputJson) {
      await writeJson(args.outputJson, payload);
    }

    process.stdout.write(`${JSON.stringify(payload, null, 2)}\n`);
  } finally {
    await mcpClient.close();
  }
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
});
