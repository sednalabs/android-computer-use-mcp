import path from "node:path";
import { promises as fs } from "node:fs";

import { createAndroidEmulatorRuntime } from "../../docs/examples/android_emulator_runtime_client.js";
import {
  createAndroidResponsesItemAdapter,
  createFunctionCallOutputItem,
} from "../../docs/examples/android_responses_items.js";
import { createSolarLabReviewSession } from "../../docs/examples/solarlab_review_session.js";
import { createMcpStreamableHttpClient } from "./mcp_streamable_http_client.js";
import { createOpenAiFileBroker } from "./openai_file_broker.js";

const DEFAULT_MODEL = "gpt-5.4";
const DEFAULT_MAX_TOOL_ROUNDS = 8;
const DEFAULT_ALLOWED_MCP_TOOLS = Object.freeze([
  "interactive_session.get_status",
  "interactive_session.get_current_build",
  "interactive_session.relaunch_current_build",
  "android.health",
  "android.list_devices",
  "android.wait_for_boot",
  "android.launch_app",
  "android.capture_screenshot",
  "android.dump_ui_hierarchy",
  "android.inspect_ui",
  "android.wait_for_stable_ui",
  "android.find_ui_element",
  "android.wait_for_ui_element",
  "android.tap_element",
  "android.type_into_element",
  "android.scroll_until_visible",
  "android.collect_logcat",
  "android.input.tap",
  "android.input.text",
  "android.input.swipe",
  "android.input.keyevent",
  "solarlab.scenario.stage_first_focus_earth",
  "solarlab.scenario.stage_first_immersive_roundtrip",
  "solarlab.semantic_action",
]);

const DEFAULT_INSTRUCTIONS = [
  "You are driving a live Android session through a host-managed Responses loop.",
  "Prefer android_observe before acting when the screen state may have changed.",
  "Prefer the dedicated Solar Lab review tools when a scripted scenario can answer the question.",
  "Use android_mcp_call only for bounded actions and observations on the live session.",
  "Observation tools return native screenshots and files; reason from those returned artifacts instead of asking for raw paths.",
].join("\n");

function ensureFetch(fetchImpl) {
  if (typeof fetchImpl !== "function") {
    throw new Error("createOpenAiResponsesLoopDriver requires a fetch implementation");
  }
  return fetchImpl;
}

function normalizeToolName(value) {
  return typeof value === "string" ? value.trim() : "";
}

function serializeJson(value) {
  return JSON.stringify(value, null, 2);
}

function summarizeStructuredResult(toolName, result) {
  return `${toolName} result\n\n${serializeJson(result)}`;
}

function flattenOutputGroups(...groups) {
  return groups.flatMap((group) => (Array.isArray(group) ? group : []));
}

function contextFromObservation(result) {
  return {
    serial: result?.serial ?? result?.device?.serial ?? null,
    screenshotPath: result?.artifacts?.screenshot_path ?? result?.screenshot_path ?? null,
    uiDumpPath: result?.artifacts?.hierarchy_path ?? result?.hierarchy_path ?? null,
    lastScenarioResult: result?.scenario ? result : null,
  };
}

function looksLikeScenarioResult(result) {
  return (
    !!result &&
    (Array.isArray(result.artifacts) ||
      typeof result.manifest_path === "string" ||
      typeof result.bundle_dir === "string")
  );
}

function looksLikeObservationResult(result) {
  const context = contextFromObservation(result);
  return Boolean(context.screenshotPath || context.uiDumpPath);
}

function isImagePath(filePath) {
  return /\.(gif|jpe?g|png|webp)$/i.test(filePath ?? "");
}

function extractOutputText(response) {
  if (typeof response?.output_text === "string" && response.output_text.trim()) {
    return response.output_text.trim();
  }

  const chunks = [];
  for (const item of Array.isArray(response?.output) ? response.output : []) {
    if (typeof item?.text === "string" && item.text.trim()) {
      chunks.push(item.text.trim());
    }
    if (!Array.isArray(item?.content)) {
      continue;
    }
    for (const content of item.content) {
      if (typeof content?.text === "string" && content.text.trim()) {
        chunks.push(content.text.trim());
      } else if (
        typeof content?.output_text === "string" &&
        content.output_text.trim()
      ) {
        chunks.push(content.output_text.trim());
      }
    }
  }

  return chunks.join("\n\n").trim();
}

function extractFunctionCalls(response) {
  return (Array.isArray(response?.output) ? response.output : []).filter(
    (item) => item?.type === "function_call",
  );
}

async function readPromptFromStream(stream) {
  const chunks = [];
  for await (const chunk of stream) {
    chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
  }
  return Buffer.concat(chunks).toString("utf8").trim();
}

async function writeJson(outputPath, payload) {
  await fs.mkdir(path.dirname(outputPath), { recursive: true });
  await fs.writeFile(outputPath, `${serializeJson(payload)}\n`);
}

function toolDefinitions(allowedMcpToolNames) {
  return [
    {
      type: "function",
      name: "android_observe",
      description:
        "Capture fresh visual context from the live Android session and return a native screenshot plus optional UI dump.",
      parameters: {
        type: "object",
        properties: {
          label: {
            type: "string",
            description: "Artifact label prefix for the captured observation.",
          },
          stable: {
            type: "boolean",
            description: "Wait for the UI to stabilize before capturing the observation.",
          },
          include_ui: {
            type: "boolean",
            description: "Include the UI hierarchy XML artifact with the screenshot.",
          },
        },
        additionalProperties: false,
      },
    },
    {
      type: "function",
      name: "solarlab_review_focus_earth",
      description:
        "Run the durable Stage First Solar Lab focus-Earth review and return native screenshots plus proof artifacts.",
      parameters: {
        type: "object",
        properties: {
          package_name: {
            type: "string",
            description: "Android package name to launch before the review.",
          },
          activity: {
            type: "string",
            description: "Optional fully qualified activity name to launch.",
          },
        },
        additionalProperties: false,
      },
    },
    {
      type: "function",
      name: "solarlab_review_immersive_roundtrip",
      description:
        "Run the durable Stage First Solar Lab immersive roundtrip review and return native screenshots plus proof artifacts.",
      parameters: {
        type: "object",
        properties: {
          package_name: {
            type: "string",
            description: "Android package name to launch before the review.",
          },
          activity: {
            type: "string",
            description: "Optional fully qualified activity name to launch.",
          },
        },
        additionalProperties: false,
      },
    },
    {
      type: "function",
      name: "android_mcp_call",
      description:
        "Call one whitelisted android-computer-use-mcp tool on the live session. Observation results return native screenshots and files instead of raw artifact paths.",
      parameters: {
        type: "object",
        properties: {
          tool_name: {
            type: "string",
            enum: [...allowedMcpToolNames],
            description: "Whitelisted MCP tool name to call.",
          },
          arguments: {
            type: "object",
            description: "JSON object passed through to the selected MCP tool.",
            additionalProperties: true,
          },
        },
        required: ["tool_name"],
        additionalProperties: false,
      },
    },
  ];
}

function createOpenAiResponsesClient({
  apiKey,
  baseUrl = "https://api.openai.com/v1",
  fetchImpl = globalThis.fetch,
} = {}) {
  if (!apiKey) {
    throw new Error("createOpenAiResponsesClient requires an apiKey");
  }

  const fetchFn = ensureFetch(fetchImpl);

  return {
    async createResponse(payload) {
      const response = await fetchFn(`${baseUrl}/responses`, {
        method: "POST",
        headers: {
          authorization: `Bearer ${apiKey}`,
          "content-type": "application/json",
        },
        body: JSON.stringify(payload),
      });

      const bodyText = await response.text();
      let json = null;
      try {
        json = JSON.parse(bodyText);
      } catch {
        json = null;
      }

      if (!response.ok) {
        throw new Error(
          `OpenAI responses.create failed with ${response.status}: ${json?.error?.message ?? bodyText}`,
        );
      }

      return json;
    },
  };
}

export function createOpenAiResponsesLoopDriver({
  model = DEFAULT_MODEL,
  instructions = DEFAULT_INSTRUCTIONS,
  maxToolRounds = DEFAULT_MAX_TOOL_ROUNDS,
  defaultPackageName = "com.sednalabs.solarlab",
  defaultActivity = null,
  defaultSerial = null,
  allowedMcpToolNames = DEFAULT_ALLOWED_MCP_TOOLS,
  mcpClient = null,
  mcpEndpoint = null,
  mcpHeaders = {},
  mcpAuthToken = null,
  openAiApiKey = null,
  openAiBaseUrl = "https://api.openai.com/v1",
  fetchImpl = globalThis.fetch,
  createResponseImpl = null,
  uploadFile = null,
  readFile = fs.readFile,
  outputDir = null,
} = {}) {
  const fetchFn = ensureFetch(fetchImpl);
  const toolNames = new Set(allowedMcpToolNames.map(normalizeToolName).filter(Boolean));
  const responsesClient =
    typeof createResponseImpl === "function"
      ? { createResponse: createResponseImpl }
      : createOpenAiResponsesClient({
          apiKey: openAiApiKey,
          baseUrl: openAiBaseUrl,
          fetchImpl: fetchFn,
        });

  const fileBroker =
    typeof uploadFile === "function"
      ? { uploadFile }
      : openAiApiKey
        ? createOpenAiFileBroker({
            apiKey: openAiApiKey,
            baseUrl: openAiBaseUrl,
            fetchImpl: fetchFn,
            readFile,
          })
        : null;

  const liveMcpClient =
    mcpClient ??
    createMcpStreamableHttpClient({
      endpoint: mcpEndpoint,
      fetchImpl: fetchFn,
      customHeaders: mcpHeaders,
      authToken: mcpAuthToken,
    });

  const responsesItems = createAndroidResponsesItemAdapter({
    readFile,
    uploadFile: fileBroker?.uploadFile ?? null,
    preferFileIdForImages: Boolean(fileBroker),
  });

  const runtime = createAndroidEmulatorRuntime({
    callMcp: (toolName, args) => liveMcpClient.callTool(toolName, args),
    displayScreenshot: async () => {},
    defaultSerial,
  });

  const reviewSession = createSolarLabReviewSession({
    callMcp: (toolName, args) => liveMcpClient.callTool(toolName, args),
    displayScreenshot: async () => {},
    defaultSerial,
    responsesItems: {
      readFile,
      uploadFile: fileBroker?.uploadFile ?? null,
      preferFileIdForImages: Boolean(fileBroker),
    },
  });

  async function traceFile(name, payload) {
    if (!outputDir) {
      return;
    }
    await writeJson(path.join(outputDir, name), payload);
  }

  async function ensureSerial() {
    if (runtime.getState().currentSerial) {
      return runtime.getState().currentSerial;
    }

    const devices = await runtime.listDevices();
    const serial = devices?.devices?.[0]?.serial ?? defaultSerial ?? null;
    if (!serial) {
      throw new Error("No Android device serial is available for the Responses loop");
    }
    await runtime.setSerial(serial);
    if (typeof reviewSession.setSerial === "function") {
      await reviewSession.setSerial(serial);
    }
    return serial;
  }

  async function toolOutputForGenericResult(callId, toolName, result) {
    if (looksLikeScenarioResult(result)) {
      return responsesItems.functionCallOutputFromScenarioResult(callId, result, {
        caption: `${toolName} result`,
      });
    }

    if (looksLikeObservationResult(result)) {
      return responsesItems.functionCallOutputFromVisualContext(
        callId,
        contextFromObservation(result),
        {
          caption: `${toolName} result`,
        },
      );
    }

    if (typeof result?.path === "string" && result.path) {
      const output = [
        {
          type: "input_text",
          text: summarizeStructuredResult(toolName, result),
        },
      ];
      if (isImagePath(result.path)) {
        output.push(await responsesItems.inputImageFromPath(result.path));
      } else {
        output.push(
          await responsesItems.inputFileFromPath(result.path, {
            filename: path.basename(result.path),
          }),
        );
      }
      return createFunctionCallOutputItem(callId, output);
    }

    return createFunctionCallOutputItem(callId, [
      {
        type: "input_text",
        text: summarizeStructuredResult(toolName, result),
      },
    ]);
  }

  async function handleObserve(call) {
    await ensureSerial();
    const args = call.args ?? {};
    const label = typeof args.label === "string" && args.label.trim()
      ? args.label.trim()
      : "openai-observe";
    const stable = args.stable !== false;
    const includeUi = args.include_ui !== false;

    if (!includeUi) {
      const shot = await runtime.captureScreenshot(label);
      return createFunctionCallOutputItem(call.callId, [
        {
          type: "input_text",
          text: summarizeStructuredResult("android_observe", {
            ok: true,
            screenshot_path: shot?.path ?? null,
            serial: runtime.getState().currentSerial,
            stable,
            include_ui: false,
          }),
        },
        await responsesItems.inputImageFromPath(shot?.path),
      ]);
    }

    const result = stable
      ? await runtime.waitForStableUi({
          hierarchyFilename: `${label}.xml`,
          screenshotFilename: `${label}.png`,
        })
      : await runtime.inspectUi({
          hierarchyFilename: `${label}.xml`,
          screenshotFilename: `${label}.png`,
        });

    return responsesItems.functionCallOutputFromVisualContext(
      call.callId,
      contextFromObservation(result),
      {
        caption: `android_observe (${stable ? "stable" : "instant"})`,
      },
    );
  }

  async function handleFocusEarthReview(call) {
    await ensureSerial();
    const args = call.args ?? {};
    const review = await reviewSession.reviewFocusEarth({
      packageName: args.package_name ?? defaultPackageName,
      activity: args.activity ?? defaultActivity,
      includeResponseItems: true,
    });

    return createFunctionCallOutputItem(
      call.callId,
      flattenOutputGroups(
        [
          {
            type: "input_text",
            text: summarizeStructuredResult("solarlab_review_focus_earth", {
              strategy: review.strategy,
              scenario: review.scripted?.scenario ?? null,
            }),
          },
        ],
        review.responseItems?.startingContext,
        review.responseItems?.scripted,
        review.responseItems?.freeformAfter,
      ),
    );
  }

  async function handleImmersiveReview(call) {
    await ensureSerial();
    const args = call.args ?? {};
    const review = await reviewSession.reviewImmersiveRoundtrip({
      packageName: args.package_name ?? defaultPackageName,
      activity: args.activity ?? defaultActivity,
      includeResponseItems: true,
    });

    return createFunctionCallOutputItem(
      call.callId,
      flattenOutputGroups(
        [
          {
            type: "input_text",
            text: summarizeStructuredResult("solarlab_review_immersive_roundtrip", {
              strategy: review.strategy,
              scenario: review.scripted?.scenario ?? null,
            }),
          },
        ],
        review.responseItems?.startingContext,
        review.responseItems?.scripted,
        review.responseItems?.freeformAfter,
      ),
    );
  }

  async function handleGenericMcpCall(call) {
    const toolName = normalizeToolName(call.args?.tool_name);
    if (!toolNames.has(toolName)) {
      throw new Error(`android_mcp_call rejected non-whitelisted tool: ${toolName}`);
    }
    const result = await liveMcpClient.callTool(toolName, call.args?.arguments ?? {});

    if (toolName === "android.list_devices") {
      const serial = result?.devices?.[0]?.serial;
      if (serial) {
        await runtime.setSerial(serial);
        if (typeof reviewSession.setSerial === "function") {
          await reviewSession.setSerial(serial);
        }
      }
    }

    return toolOutputForGenericResult(call.callId, toolName, result);
  }

  const toolHandlers = new Map([
    ["android_observe", handleObserve],
    ["solarlab_review_focus_earth", handleFocusEarthReview],
    ["solarlab_review_immersive_roundtrip", handleImmersiveReview],
    ["android_mcp_call", handleGenericMcpCall],
  ]);

  return {
    getToolDefinitions() {
      return toolDefinitions([...toolNames]);
    },

    async run({
      prompt = null,
      input = null,
      promptStream = null,
      responseInstructions = instructions,
    } = {}) {
      const resolvedPrompt =
        prompt ??
        (input == null && promptStream ? await readPromptFromStream(promptStream) : null);

      const initialInput =
        input ??
        (resolvedPrompt
          ? [
              {
                role: "user",
                content: [{ type: "input_text", text: resolvedPrompt }],
              },
            ]
          : null);

      if (!Array.isArray(initialInput) || initialInput.length === 0) {
        throw new Error("run requires either prompt text or input items");
      }

      await liveMcpClient.initialize();
      await traceFile("run-request.json", {
        model,
        instructions: responseInstructions,
        input: initialInput,
      });

      let response = await responsesClient.createResponse({
        model,
        instructions: responseInstructions,
        input: initialInput,
        tools: this.getToolDefinitions(),
      });
      await traceFile("turn-01-response.json", response);

      let previousResponseId = response?.id ?? null;
      let rounds = 0;
      const handledCalls = [];

      while (rounds < maxToolRounds) {
        const functionCalls = extractFunctionCalls(response);
        if (functionCalls.length === 0) {
          break;
        }

        const outputs = [];
        for (const item of functionCalls) {
          const handler = toolHandlers.get(item.name);
          if (!handler) {
            throw new Error(`Responses loop received unsupported function call: ${item.name}`);
          }
          const call = {
            name: item.name,
            callId: item.call_id,
            args:
              typeof item.arguments === "string" && item.arguments.trim()
                ? JSON.parse(item.arguments)
                : item.arguments ?? {},
          };
          const output = await handler(call);
          outputs.push(output);
          handledCalls.push({
            name: call.name,
            call_id: call.callId,
          });
        }

        rounds += 1;
        await traceFile(`turn-${String(rounds).padStart(2, "0")}-tool-outputs.json`, outputs);

        response = await responsesClient.createResponse({
          model,
          previous_response_id: previousResponseId,
          input: outputs,
          tools: this.getToolDefinitions(),
        });
        previousResponseId = response?.id ?? previousResponseId;
        await traceFile(`turn-${String(rounds + 1).padStart(2, "0")}-response.json`, response);
      }

      if (rounds >= maxToolRounds && extractFunctionCalls(response).length > 0) {
        throw new Error(`Responses loop exceeded maxToolRounds=${maxToolRounds}`);
      }

      const result = {
        model,
        response,
        output_text: extractOutputText(response),
        function_calls_handled: handledCalls,
      };
      await traceFile("run-result.json", result);
      return result;
    },

    async close() {
      await liveMcpClient.close();
    },
  };
}

export {
  DEFAULT_ALLOWED_MCP_TOOLS,
  DEFAULT_INSTRUCTIONS,
  DEFAULT_MAX_TOOL_ROUNDS,
  DEFAULT_MODEL,
  createOpenAiResponsesClient,
};
