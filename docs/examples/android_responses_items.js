import path from "node:path";
import { promises as fs } from "node:fs";

const MIME_BY_EXTENSION = Object.freeze({
  ".gif": "image/gif",
  ".jpeg": "image/jpeg",
  ".jpg": "image/jpeg",
  ".json": "application/json",
  ".log": "text/plain",
  ".md": "text/markdown",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".txt": "text/plain",
  ".webp": "image/webp",
  ".xml": "application/xml",
});

function guessMimeType(filePath, fallback = "application/octet-stream") {
  if (!filePath) {
    return fallback;
  }
  return MIME_BY_EXTENSION[path.extname(filePath).toLowerCase()] ?? fallback;
}

function toDataUrl(data, mimeType) {
  return `data:${mimeType};base64,${Buffer.from(data).toString("base64")}`;
}

function basename(filePath, fallback) {
  return filePath ? path.basename(filePath) : fallback;
}

function finalScenarioArtifact(result) {
  const artifacts = Array.isArray(result?.artifacts) ? result.artifacts : [];
  return artifacts.at(-1) ?? null;
}

function formatVisualContextSummary(context, caption) {
  const lines = [];
  if (caption) {
    lines.push(caption);
  }
  lines.push("Android visual context");
  if (context?.serial) {
    lines.push(`serial: ${context.serial}`);
  }
  if (context?.screenshotPath) {
    lines.push(`screenshot: ${context.screenshotPath}`);
  }
  if (context?.uiDumpPath) {
    lines.push(`ui dump: ${context.uiDumpPath}`);
  }
  if (context?.lastScenarioResult?.scenario) {
    lines.push(`last scenario: ${context.lastScenarioResult.scenario}`);
  }
  return lines.join("\n");
}

function formatScenarioSummary(result, caption) {
  const lines = [];
  if (caption) {
    lines.push(caption);
  }
  lines.push("Android scenario result");
  if (result?.scenario) {
    lines.push(`scenario: ${result.scenario}`);
  }
  if (result?.bundle_dir) {
    lines.push(`bundle dir: ${result.bundle_dir}`);
  }
  if (result?.manifest_path) {
    lines.push(`manifest: ${result.manifest_path}`);
  }
  const artifact = finalScenarioArtifact(result);
  if (artifact?.screenshot) {
    lines.push(`final screenshot: ${artifact.screenshot}`);
  }
  if (artifact?.ui_dump) {
    lines.push(`final ui dump: ${artifact.ui_dump}`);
  }
  if (result?.logcat_path) {
    lines.push(`logcat: ${result.logcat_path}`);
  }
  return lines.join("\n");
}

export function createFunctionCallOutputItem(callId, output) {
  if (!callId) {
    throw new Error("createFunctionCallOutputItem requires a call id");
  }
  if (!Array.isArray(output)) {
    throw new Error("createFunctionCallOutputItem requires output to be an array");
  }
  return {
    type: "function_call_output",
    call_id: callId,
    output,
  };
}

export function createComputerCallOutputItem(
  callId,
  imageUrl,
  { detail = "original" } = {},
) {
  if (!callId) {
    throw new Error("createComputerCallOutputItem requires a call id");
  }
  if (!imageUrl) {
    throw new Error("createComputerCallOutputItem requires an image URL");
  }
  return {
    type: "computer_call_output",
    call_id: callId,
    output: {
      type: "computer_screenshot",
      image_url: imageUrl,
      detail,
    },
  };
}

export function createAndroidResponsesItemAdapter({
  readFile = fs.readFile,
  imageUrlForPath = null,
  fileUrlForPath = null,
  uploadFile = null,
  preferFileIdForImages = false,
} = {}) {
  if (typeof readFile !== "function") {
    throw new Error("createAndroidResponsesItemAdapter requires a readFile function");
  }

  async function inputImageFromPath(
    filePath,
    {
      detail = "original",
      filename = basename(filePath, "image.png"),
      mimeType = guessMimeType(filePath, "image/png"),
      purpose = "user_data",
    } = {},
  ) {
    if (!filePath) {
      throw new Error("inputImageFromPath requires a file path");
    }
    if (preferFileIdForImages && typeof uploadFile === "function") {
      const uploaded = await uploadFile(filePath, { filename, mimeType, purpose });
      if (uploaded?.file_id) {
        return { type: "input_image", file_id: uploaded.file_id, detail };
      }
      if (uploaded?.file_url) {
        return { type: "input_image", image_url: uploaded.file_url, detail };
      }
      throw new Error("uploadFile must return either { file_id } or { file_url }");
    }
    if (typeof imageUrlForPath === "function") {
      const image_url = await imageUrlForPath(filePath, { mimeType, detail });
      return { type: "input_image", image_url, detail };
    }
    const bytes = await readFile(filePath);
    return {
      type: "input_image",
      image_url: toDataUrl(bytes, mimeType),
      detail,
    };
  }

  async function inputFileFromPath(
    filePath,
    {
      filename = basename(filePath, "artifact.bin"),
      mimeType = guessMimeType(filePath),
      purpose = "user_data",
    } = {},
  ) {
    if (!filePath) {
      throw new Error("inputFileFromPath requires a file path");
    }

    if (typeof uploadFile === "function") {
      const uploaded = await uploadFile(filePath, { filename, mimeType, purpose });
      if (uploaded?.file_id) {
        return { type: "input_file", file_id: uploaded.file_id };
      }
      if (uploaded?.file_url) {
        return { type: "input_file", file_url: uploaded.file_url };
      }
      throw new Error("uploadFile must return either { file_id } or { file_url }");
    }

    if (typeof fileUrlForPath === "function") {
      const file_url = await fileUrlForPath(filePath, { filename, mimeType, purpose });
      return { type: "input_file", file_url };
    }

    const bytes = await readFile(filePath);
    return {
      type: "input_file",
      filename,
      file_data: toDataUrl(bytes, mimeType),
    };
  }

  async function visualContextToInputItems(
    context,
    {
      caption = null,
      includeTextSummary = true,
      includeUiDump = true,
      screenshotDetail = "original",
    } = {},
  ) {
    const items = [];
    if (includeTextSummary) {
      items.push({
        type: "input_text",
        text: formatVisualContextSummary(context, caption),
      });
    }
    if (context?.screenshotPath) {
      items.push(await inputImageFromPath(context.screenshotPath, { detail: screenshotDetail }));
    }
    if (includeUiDump && context?.uiDumpPath) {
      items.push(
        await inputFileFromPath(context.uiDumpPath, {
          filename: basename(context.uiDumpPath, "android-ui.xml"),
          mimeType: "application/xml",
        }),
      );
    }
    return items;
  }

  async function scenarioResultToInputItems(
    result,
    {
      caption = null,
      includeTextSummary = true,
      includeManifest = true,
      includeFinalScreenshot = true,
      includeFinalUiDump = true,
      includeLogcat = true,
      screenshotDetail = "original",
    } = {},
  ) {
    const items = [];
    if (includeTextSummary) {
      items.push({
        type: "input_text",
        text: formatScenarioSummary(result, caption),
      });
    }

    const artifact = finalScenarioArtifact(result);
    if (includeFinalScreenshot && artifact?.screenshot) {
      items.push(await inputImageFromPath(artifact.screenshot, { detail: screenshotDetail }));
    }
    if (includeFinalUiDump && artifact?.ui_dump) {
      items.push(
        await inputFileFromPath(artifact.ui_dump, {
          filename: basename(artifact.ui_dump, "scenario-final.xml"),
          mimeType: "application/xml",
        }),
      );
    }
    if (includeManifest && result?.manifest_path) {
      items.push(
        await inputFileFromPath(result.manifest_path, {
          filename: basename(result.manifest_path, "manifest.json"),
          mimeType: "application/json",
        }),
      );
    }
    if (includeLogcat && result?.logcat_path) {
      items.push(
        await inputFileFromPath(result.logcat_path, {
          filename: basename(result.logcat_path, "android-logcat.txt"),
          mimeType: "text/plain",
        }),
      );
    }
    return items;
  }

  async function functionCallOutputFromVisualContext(
    callId,
    context,
    options = {},
  ) {
    return createFunctionCallOutputItem(
      callId,
      await visualContextToInputItems(context, options),
    );
  }

  async function functionCallOutputFromScenarioResult(
    callId,
    result,
    options = {},
  ) {
    return createFunctionCallOutputItem(
      callId,
      await scenarioResultToInputItems(result, options),
    );
  }

  async function computerCallOutputFromPath(
    callId,
    filePath,
    {
      detail = "original",
      mimeType = guessMimeType(filePath, "image/png"),
    } = {},
  ) {
    const item = await inputImageFromPath(filePath, { detail, mimeType });
    return createComputerCallOutputItem(callId, item.image_url, { detail: item.detail });
  }

  return {
    inputImageFromPath,
    inputFileFromPath,
    visualContextToInputItems,
    scenarioResultToInputItems,
    functionCallOutputFromVisualContext,
    functionCallOutputFromScenarioResult,
    computerCallOutputFromPath,
  };
}
