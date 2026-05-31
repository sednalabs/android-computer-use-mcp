import { promises as fs } from "node:fs";

import { createAndroidResponsesItemAdapter } from "../../docs/examples/android_responses_items.js";

export function createMessageItem(content, { role = "assistant" } = {}) {
  if (!Array.isArray(content)) {
    throw new Error("createMessageItem requires content to be an array");
  }
  if (!role || typeof role !== "string") {
    throw new Error("createMessageItem requires a role string");
  }
  return {
    type: "message",
    role,
    content,
  };
}

export function createThreadInjectItemsParams(threadId, items) {
  if (!threadId || typeof threadId !== "string") {
    throw new Error("createThreadInjectItemsParams requires a thread id");
  }
  if (!Array.isArray(items)) {
    throw new Error("createThreadInjectItemsParams requires an items array");
  }
  return {
    threadId,
    items,
  };
}

export function contextFromObservation(result) {
  return {
    serial: result?.serial ?? null,
    screenshotPath: result?.artifacts?.screenshot_path ?? null,
    uiDumpPath: result?.artifacts?.hierarchy_path ?? null,
    lastScenarioResult: null,
  };
}

export function createCodexThreadItemsAdapter({
  readFile = fs.readFile,
  imageUrlForPath = null,
  uploadFile = null,
  preferFileIdForImages = false,
} = {}) {
  const responsesItems = createAndroidResponsesItemAdapter({
    readFile,
    imageUrlForPath,
    uploadFile,
    preferFileIdForImages,
  });

  async function visualContextMessage(
    context,
    {
      role = "assistant",
      caption = "Hosted Android observation",
      includeTextSummary = true,
      screenshotDetail = "original",
    } = {},
  ) {
    const content = await responsesItems.visualContextToInputItems(context, {
      caption,
      includeTextSummary,
      includeUiDump: false,
      screenshotDetail,
    });
    return createMessageItem(content, { role });
  }

  async function scenarioResultMessage(
    result,
    {
      role = "assistant",
      caption = "Hosted Android scenario observation",
      includeTextSummary = true,
      screenshotDetail = "original",
    } = {},
  ) {
    const content = await responsesItems.scenarioResultToInputItems(result, {
      caption,
      includeTextSummary,
      includeManifest: false,
      includeFinalScreenshot: true,
      includeFinalUiDump: false,
      includeLogcat: false,
      screenshotDetail,
    });
    return createMessageItem(content, { role });
  }

  async function threadInjectItemsFromVisualContext(threadId, context, options = {}) {
    return createThreadInjectItemsParams(threadId, [
      await visualContextMessage(context, options),
    ]);
  }

  async function threadInjectItemsFromScenarioResult(threadId, result, options = {}) {
    return createThreadInjectItemsParams(threadId, [
      await scenarioResultMessage(result, options),
    ]);
  }

  return {
    ...responsesItems,
    createMessageItem,
    createThreadInjectItemsParams,
    contextFromObservation,
    visualContextMessage,
    scenarioResultMessage,
    threadInjectItemsFromVisualContext,
    threadInjectItemsFromScenarioResult,
  };
}
