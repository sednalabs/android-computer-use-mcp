#!/usr/bin/env node

import path from "node:path";
import { promises as fs } from "node:fs";

import {
  DEFAULT_MAX_TOOL_ROUNDS,
  DEFAULT_MODEL,
  createOpenAiResponsesLoopDriver,
} from "../index.js";

function usage() {
  return `Usage: openai-android-loop.mjs [options]

Runs a thin OpenAI Responses loop against a live android-computer-use-mcp endpoint
and returns native screenshot/file outputs from tool calls.

Options:
  --config PATH               JSON config file written by the host workflow
  --prompt TEXT               Inline user prompt
  --prompt-file PATH          Read the user prompt from a file
  --instructions-file PATH    Read additional loop instructions from a file
  --model NAME                Responses model to use (default: ${DEFAULT_MODEL})
  --mcp-url URL               MCP streamable HTTP endpoint
  --mcp-header NAME=VALUE     Extra MCP request header (repeatable)
  --serial SERIAL             Default Android serial to target
  --output-dir PATH           Directory for loop traces and result payloads
  --max-tool-rounds N         Maximum tool-call rounds (default: ${DEFAULT_MAX_TOOL_ROUNDS})
  -h, --help                  Show this help
`;
}

async function loadJson(filePath) {
  return JSON.parse(await fs.readFile(filePath, "utf8"));
}

async function loadText(filePath) {
  return fs.readFile(filePath, "utf8");
}

function parseArgs(argv) {
  const parsed = {
    mcpHeaders: {},
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    switch (arg) {
      case "--config":
        parsed.configPath = argv[++index];
        break;
      case "--prompt":
        parsed.prompt = argv[++index];
        break;
      case "--prompt-file":
        parsed.promptFile = argv[++index];
        break;
      case "--instructions-file":
        parsed.instructionsFile = argv[++index];
        break;
      case "--model":
        parsed.model = argv[++index];
        break;
      case "--mcp-url":
        parsed.mcpUrl = argv[++index];
        break;
      case "--mcp-header": {
        const raw = argv[++index];
        const separator = raw.indexOf("=");
        if (separator <= 0) {
          throw new Error(`--mcp-header requires NAME=VALUE, got: ${raw}`);
        }
        parsed.mcpHeaders[raw.slice(0, separator)] = raw.slice(separator + 1);
        break;
      }
      case "--serial":
        parsed.serial = argv[++index];
        break;
      case "--output-dir":
        parsed.outputDir = argv[++index];
        break;
      case "--max-tool-rounds":
        parsed.maxToolRounds = Number.parseInt(argv[++index], 10);
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

function mergeHeaders(configHeaders = {}, argHeaders = {}) {
  return {
    ...configHeaders,
    ...argHeaders,
  };
}

function timestampedRunDir(outputRoot) {
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  return path.join(outputRoot, `run-${stamp}`);
}

async function resolvePrompt(args) {
  if (args.prompt) {
    return args.prompt;
  }
  if (args.promptFile) {
    return (await loadText(args.promptFile)).trim();
  }
  if (!process.stdin.isTTY) {
    const chunks = [];
    for await (const chunk of process.stdin) {
      chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
    }
    return Buffer.concat(chunks).toString("utf8").trim();
  }
  throw new Error("A prompt is required via --prompt, --prompt-file, or stdin");
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    process.stdout.write(usage());
    return;
  }

  const config = args.configPath ? await loadJson(args.configPath) : {};
  const prompt = await resolvePrompt(args);
  const instructions = args.instructionsFile
    ? await loadText(args.instructionsFile)
    : config.instructions ?? null;
  const outputDir =
    args.outputDir ??
    (config.output_root ? timestampedRunDir(config.output_root) : null);

  const driver = createOpenAiResponsesLoopDriver({
    model: args.model ?? config.default_model ?? process.env.OPENAI_MODEL ?? DEFAULT_MODEL,
    instructions: instructions ?? undefined,
    maxToolRounds:
      args.maxToolRounds ??
      config.max_tool_rounds ??
      DEFAULT_MAX_TOOL_ROUNDS,
    defaultSerial:
      args.serial ??
      config.default_serial ??
      process.env.ANDROID_SERIAL ??
      "emulator-5554",
    defaultPackageName:
      config.default_package_name ?? "com.sednalabs.solarlab",
    defaultActivity: config.default_activity ?? null,
    mcpEndpoint: args.mcpUrl ?? config.mcp_url,
    mcpHeaders: mergeHeaders(config.mcp_headers ?? {}, args.mcpHeaders),
    openAiApiKey: process.env.OPENAI_API_KEY,
    outputDir,
  });

  try {
    const result = await driver.run({ prompt });
    if (outputDir) {
      await fs.mkdir(outputDir, { recursive: true });
      await fs.writeFile(
        path.join(outputDir, "run-summary.md"),
        [
          "# OpenAI Android Loop",
          "",
          `- model: \`${result.model}\``,
          `- output dir: \`${outputDir}\``,
          `- function calls handled: \`${result.function_calls_handled.length}\``,
          "",
          "## Final Text",
          "",
          result.output_text || "_No assistant text returned._",
          "",
        ].join("\n"),
      );
    }
    process.stdout.write(`${result.output_text || ""}\n`);
  } finally {
    await driver.close();
  }
}

main().catch((error) => {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
});
