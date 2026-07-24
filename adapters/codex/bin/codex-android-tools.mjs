#!/usr/bin/env node

import { promises as fs } from "node:fs";

import { createAndroidEmulatorRuntime } from "../../../docs/examples/android_emulator_runtime_client.js";
import { createMcpStreamableHttpClient } from "../../openai/mcp_streamable_http_client.js";
import {
  createCodexAndroidDynamicToolHost,
  createCodexAndroidProviderManifest,
  validateCodexAndroidProviderManifest,
} from "../index.js";
import {
  loadCodexAdapterConfig,
  requiredOptionValue,
  resolveMcpHeaders,
  resolveMcpUrl,
} from "../cli_common.js";

function usage() {
  console.log(`Usage: codex-android-tools.mjs [options] <specs|call|manifest|validate-manifest>

Codex-native Android dynamic tool host. This command does not call OpenAI
directly. It exposes model-callable Android tools over a small JSON contract
for the Codex TUI dynamic-tool bridge.

Options:
  --config PATH               Shared hosted-session config JSON
  --mcp-url URL               MCP streamable HTTP endpoint
  --mcp-header NAME=VALUE     Extra MCP request header (repeatable)
  --serial SERIAL             Android serial to target
  --default-package-name PKG  Default app package for launch_app
  --default-activity ACT      Default activity for launch_app
  --environment-id ID         Exact Android provider environment identity
  --provider-instance-id ID   Exact Android provider instance identity
  --session-id ID             Exact Android provider session identity
  --session-root PATH         Hosted-session root for provider manifest output
  --artifact-root PATH        Artifact root for provider manifest output
  --build-manifest PATH       APK/build manifest path for provider manifest output
  --manifest PATH             Provider manifest path for validate-manifest
  -h, --help                  Show this help
`);
}

function parseArgs(argv) {
  const parsed = {
    mcpHeaders: [],
    command: null,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    switch (arg) {
      case "--config":
        parsed.configPath = requiredOptionValue(argv, index++);
        break;
      case "--mcp-url":
        parsed.mcpUrl = requiredOptionValue(argv, index++);
        break;
      case "--mcp-header":
        parsed.mcpHeaders.push(requiredOptionValue(argv, index++));
        break;
      case "--serial":
        parsed.serial = requiredOptionValue(argv, index++);
        break;
      case "--default-package-name":
        parsed.defaultPackageName = requiredOptionValue(argv, index++);
        break;
      case "--default-activity":
        parsed.defaultActivity = requiredOptionValue(argv, index++);
        break;
      case "--environment-id":
        parsed.environmentId = requiredOptionValue(argv, index++);
        break;
      case "--provider-instance-id":
        parsed.providerInstanceId = requiredOptionValue(argv, index++);
        break;
      case "--session-id":
        parsed.sessionId = requiredOptionValue(argv, index++);
        break;
      case "--session-root":
        parsed.sessionRoot = requiredOptionValue(argv, index++);
        break;
      case "--artifact-root":
        parsed.artifactRoot = requiredOptionValue(argv, index++);
        break;
      case "--build-manifest":
        parsed.buildManifestPath = requiredOptionValue(argv, index++);
        break;
      case "--manifest":
        parsed.manifestPath = requiredOptionValue(argv, index++);
        break;
      case "-h":
      case "--help":
        parsed.help = true;
        break;
      case "specs":
      case "call":
      case "manifest":
      case "validate-manifest":
        parsed.command = arg;
        break;
      default:
        throw new Error(`Unknown argument: ${arg}`);
    }
  }

  return parsed;
}

async function readJsonFromStdin() {
  const chunks = [];
  for await (const chunk of process.stdin) {
    chunks.push(chunk);
  }
  const raw = Buffer.concat(chunks).toString("utf8").trim();
  if (!raw) {
    throw new Error("codex-android-tools call requires JSON on stdin");
  }
  return JSON.parse(raw);
}

async function readJsonFromPathOrStdin(filePath) {
  if (filePath) {
    return JSON.parse(await fs.readFile(filePath, "utf8"));
  }
  return readJsonFromStdin();
}

function normalizeReadEncoding(encoding) {
  if (typeof encoding === "string" && encoding.trim()) {
    return encoding.trim();
  }
  if (encoding && typeof encoding === "object" && typeof encoding.encoding === "string") {
    return encoding.encoding.trim();
  }
  return null;
}

async function readArtifactViaMcp(mcpClient, filePath, encoding = undefined) {
  const normalizedEncoding = normalizeReadEncoding(encoding);
  const remoteEncoding =
    normalizedEncoding === "utf8" || normalizedEncoding === "utf-8"
      ? "utf8"
      : "base64";
  const result = await mcpClient.callTool("android.read_artifact", {
    path: filePath,
    encoding: remoteEncoding,
  });

  if (remoteEncoding === "utf8") {
    if (typeof result?.text !== "string") {
      throw new Error("android.read_artifact utf8 response did not include text");
    }
    return result.text;
  }

  if (typeof result?.data_base64 !== "string") {
    throw new Error("android.read_artifact base64 response did not include data_base64");
  }
  return Buffer.from(result.data_base64, "base64");
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help || !args.command) {
    usage();
    if (!args.help) {
      process.exitCode = 1;
    }
    return;
  }

  if (args.command === "validate-manifest") {
    const manifest = await readJsonFromPathOrStdin(args.manifestPath);
    process.stdout.write(
      `${JSON.stringify(validateCodexAndroidProviderManifest(manifest), null, 2)}\n`,
    );
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
      "codex-android-tools requires --mcp-url, a config with mcp_url, or CODEX_ANDROID_MCP_URL/CODEX_ANDROID_MCP_HOSTNAME",
    );
  }

  const defaultSerial = args.serial ?? config.default_serial ?? null;
  const defaultPackageName =
    args.defaultPackageName ??
    config.default_package_name ??
    null;
  const defaultActivity =
    args.defaultActivity ??
    config.default_activity ??
    null;

  if (args.command === "manifest") {
    process.stdout.write(`${JSON.stringify(createCodexAndroidProviderManifest({
      mcpUrl: endpoint,
      defaultSerial,
      defaultPackageName,
      defaultActivity,
      environmentId: args.environmentId ?? config.environment_id ?? "local",
      providerInstanceId:
        args.providerInstanceId ??
        config.provider_instance_id ??
        "android-computer-use-mcp",
      sessionId: args.sessionId ?? config.session_id ?? "default",
      sessionRoot: args.sessionRoot ?? config.session_root ?? null,
      artifactRoot: args.artifactRoot ?? config.artifact_root ?? null,
      buildManifestPath: args.buildManifestPath ?? config.build_manifest_path ?? null,
    }), null, 2)}\n`);
    return;
  }

  const mcpClient = createMcpStreamableHttpClient({
    endpoint,
    customHeaders: resolveMcpHeaders({
      headerArgs: args.mcpHeaders,
    }),
  });

  try {
    const runtime = createAndroidEmulatorRuntime({
      callMcp: (toolName, toolArgs) => mcpClient.callTool(toolName, toolArgs),
      displayScreenshot: async () => {},
      defaultSerial,
    });

    const host = createCodexAndroidDynamicToolHost({
      runtime,
      readFile: fs.readFile,
      readRemoteArtifactFile: (filePath, encoding) =>
        readArtifactViaMcp(mcpClient, filePath, encoding),
      defaultPackageName,
      defaultActivity,
    });

    const payload =
      args.command === "specs"
        ? host.getToolSpecs()
        : await host.executeToolCall(await readJsonFromStdin());

    process.stdout.write(`${JSON.stringify(payload, null, 2)}\n`);
  } finally {
    await mcpClient.close();
  }
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
});
