import path from "node:path";
import { promises as fs } from "node:fs";

import { createAndroidComputerBridge } from "../../docs/examples/android_computer_bridge.js";
import { createAndroidResponsesItemAdapter } from "../../docs/examples/android_responses_items.js";
import { contextFromObservation } from "./codex_thread_items.js";

const OBSERVE_SCOPE_VALUES = Object.freeze(["screen", "screen_and_ui"]);
const TOOL_ANDROID_OBSERVE = "android_observe";
const TOOL_ANDROID_STEP = "android_step";
const TOOL_ANDROID_INSTALL_BUILD_FROM_RUN = "android_install_build_from_run";
const LEGACY_STEP_ACTION_VALUES = Object.freeze([
  "launch_app",
  "tap",
  "type_text",
  "key",
  "swipe",
  "multi_touch",
  "wait",
  "open_url",
  "set_orientation",
  "long_press",
  "semantic_action",
]);
const COMPUTER_ACTION_VALUES = Object.freeze([
  "launch_app",
  "click",
  "double_click",
  "long_press",
  "scroll",
  "type",
  "wait",
  "keypress",
  "drag",
  "open_url",
  "set_orientation",
  "move",
  "zoom",
  "reset_zoom",
  "semantic_action",
]);
const STEP_ACTION_VALUES = Object.freeze([
  ...new Set([...LEGACY_STEP_ACTION_VALUES, ...COMPUTER_ACTION_VALUES]),
]);
const BRIDGE_BATCH_ACTION_VALUES = new Set([
  "click",
  "double_click",
  "long_press",
  "scroll",
  "type",
  "wait",
  "keypress",
  "drag",
  "zoom",
  "reset_zoom",
]);
const VIEW_GEOMETRY_INVALIDATING_ACTION_VALUES = new Set([
  "set_orientation",
]);

function compactJson(value, fallback = "") {
  if (value == null) {
    return fallback;
  }
  const rendered = JSON.stringify(value);
  return rendered.length > 160 ? `${rendered.slice(0, 157)}...` : rendered;
}

function normalizeCue(value) {
  if (typeof value !== "string") {
    return null;
  }
  const trimmed = value.replace(/\s+/g, " ").trim();
  if (!trimmed) {
    return null;
  }
  if (trimmed.includes("/")) {
    return trimmed.slice(trimmed.lastIndexOf("/") + 1);
  }
  return trimmed;
}

function compactCue(value, limit = 48) {
  if (typeof value !== "string" || value.length <= limit) {
    return value;
  }
  return `${value.slice(0, limit - 3)}...`;
}

function summarizeObserveError(error) {
  const message = error instanceof Error ? error.message : String(error);
  return message.length > 160 ? `${message.slice(0, 157)}...` : message;
}

function isMissingLocalArtifactError(error) {
  return error?.code === "ENOENT" || error?.code === "ENOTDIR";
}

function shouldPreferRemoteArtifactRead(filePath) {
  return typeof filePath === "string" && !path.isAbsolute(filePath);
}

async function summarizeUiDump(uiDumpPath, readFile) {
  if (!uiDumpPath) {
    return null;
  }

  const xml = await readFile(uiDumpPath, "utf8");
  const nodes = parseUiDumpNodes(xml);
  const nodeCount = nodes.length;
  const cues = [];
  const seen = new Set();
  const patterns = [
    /\btext="([^"]*)"/g,
    /\bcontent-desc="([^"]*)"/g,
    /\bresource-id="([^"]*)"/g,
  ];

  for (const pattern of patterns) {
    for (const match of xml.matchAll(pattern)) {
      const cue = normalizeCue(match[1]);
      if (!cue || seen.has(cue)) {
        continue;
      }
      seen.add(cue);
      cues.push(cue);
      if (cues.length >= 8) {
        break;
      }
    }
    if (cues.length >= 8) {
      break;
    }
  }

  const summary = [`ui digest: ${nodeCount} nodes`];
  if (cues.length > 0) {
    summary.push(`cues: ${cues.join(" | ")}`);
  }
  const visibleSummary = summarizeVisibleUiNodes(nodes);
  if (visibleSummary) {
    summary.push(visibleSummary);
  }
  const scrollableCount = nodes.filter((node) => node.scrollable === "true").length;
  if (scrollableCount > 0) {
    summary.push(`scrollable_nodes: ${scrollableCount}`);
  }
  return summary.join("; ");
}

function parseUiDumpNodes(xml) {
  const nodes = [];
  for (const nodeMatch of xml.matchAll(/<node\b([^>]*)>/g)) {
    const rawAttributes = nodeMatch[1] ?? "";
    const attributes = {};
    for (const attrMatch of rawAttributes.matchAll(/\b([a-zA-Z0-9_-]+)="([^"]*)"/g)) {
      attributes[attrMatch[1]] = attrMatch[2];
    }
    nodes.push(attributes);
  }
  return nodes;
}

function summarizeVisibleUiNodes(nodes) {
  const viewport = inferViewportBounds(nodes);
  const entries = [];
  let clippedCount = 0;

  for (const node of nodes) {
    const label = normalizeCue(node.text) ?? normalizeCue(node["content-desc"]) ?? normalizeCue(node["resource-id"]);
    const bounds = parseBounds(node.bounds);
    if (!label || !bounds) {
      continue;
    }

    const visibility = viewport ? visibilityWithinViewport(bounds, viewport) : null;
    if (visibility?.clipped) {
      clippedCount += 1;
    }

    entries.push(`${compactCueWithState(label, node, visibility)} ${formatBounds(bounds)}`);
    if (entries.length >= 8) {
      break;
    }
  }

  if (entries.length === 0) {
    return null;
  }

  const clippedSummary = clippedCount > 0 ? `; clipped_nodes: ${clippedCount}` : "";
  return `visible_ui: ${entries.join(" | ")}${clippedSummary}`;
}

function compactCueWithState(label, node, visibility) {
  const compacted = compactCue(label);
  const tags = [];
  if (node.enabled === "false") {
    tags.push("disabled");
  }
  if (node.scrollable === "true" && !label.toLowerCase().includes("scrollable")) {
    tags.push("scrollable");
  }
  if (visibility?.clipped) {
    const edges = visibility.clipEdges.length > 0 ? ` ${visibility.clipEdges.join("/")}` : "";
    const fraction = visibility.visibleFractionPercent < 100 ? ` ${visibility.visibleFractionPercent}%` : "";
    tags.push(`clipped${edges}${fraction}`);
  }
  if (tags.length === 0) {
    return compacted;
  }
  return `${compacted} [${tags.join("; ")}]`;
}

function inferViewportBounds(nodes) {
  const bounds = nodes
    .map((node) => parseBounds(node.bounds))
    .filter(Boolean);
  const zeroOrigin = bounds.filter((candidate) => candidate.left === 0 && candidate.top === 0);
  return maxAreaBounds(zeroOrigin) ?? maxAreaBounds(bounds);
}

function maxAreaBounds(bounds) {
  let selected = null;
  let selectedArea = -1;
  for (const candidate of bounds) {
    const area = boundsArea(candidate);
    if (area > selectedArea) {
      selected = candidate;
      selectedArea = area;
    }
  }
  return selected;
}

function parseBounds(raw) {
  if (typeof raw !== "string") {
    return null;
  }
  const match = raw.match(/^\[(\d+),(\d+)\]\[(\d+),(\d+)\]$/);
  if (!match) {
    return null;
  }
  return {
    left: Number(match[1]),
    top: Number(match[2]),
    right: Number(match[3]),
    bottom: Number(match[4]),
  };
}

function formatBounds(bounds) {
  return `[${bounds.left},${bounds.top}][${bounds.right},${bounds.bottom}]`;
}

function visibilityWithinViewport(bounds, viewport) {
  const clipEdges = [];
  if (bounds.left < viewport.left) {
    clipEdges.push("left");
  }
  if (bounds.top < viewport.top) {
    clipEdges.push("top");
  }
  if (bounds.right > viewport.right) {
    clipEdges.push("right");
  }
  if (bounds.bottom > viewport.bottom) {
    clipEdges.push("bottom");
  }

  const area = boundsArea(bounds);
  const visibleArea = intersectionArea(bounds, viewport);
  const visibleFractionPercent = area === 0
    ? 0
    : Math.min(100, Math.round((visibleArea * 100) / area));
  return {
    clipped: clipEdges.length > 0 || visibleFractionPercent < 100,
    clipEdges,
    visibleFractionPercent,
  };
}

function boundsArea(bounds) {
  return Math.max(0, bounds.right - bounds.left) * Math.max(0, bounds.bottom - bounds.top);
}

function intersectionArea(leftBounds, rightBounds) {
  const left = Math.max(leftBounds.left, rightBounds.left);
  const top = Math.max(leftBounds.top, rightBounds.top);
  const right = Math.min(leftBounds.right, rightBounds.right);
  const bottom = Math.min(leftBounds.bottom, rightBounds.bottom);
  return Math.max(0, right - left) * Math.max(0, bottom - top);
}

function toDynamicToolContentItem(item) {
  switch (item?.type) {
    case "input_text":
      return {
        type: "inputText",
        text: item.text,
      };
    case "input_image":
      if (typeof item.image_url !== "string") {
        throw new Error("dynamic tool image items require image_url");
      }
      return {
        type: "inputImage",
        imageUrl: item.image_url,
        detail: item.detail ?? "original",
      };
    default:
      throw new Error(`Unsupported content item type for dynamic tools: ${item?.type}`);
  }
}

function createDynamicToolCallResponse(contentItems, { success = true, metadata = null } = {}) {
  if (!Array.isArray(contentItems)) {
    throw new Error("createDynamicToolCallResponse requires a contentItems array");
  }
  const response = {
    contentItems,
    success,
  };
  if (metadata && typeof metadata === "object" && !Array.isArray(metadata)) {
    response.metadata = metadata;
  }
  return response;
}

function requireObject(value, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  return value;
}

function requireNumber(value, label) {
  if (typeof value !== "number" || Number.isNaN(value)) {
    throw new Error(`${label} must be a number`);
  }
  return value;
}

function requirePositiveInteger(value, label) {
  if (!Number.isInteger(value) || value <= 0) {
    throw new Error(`${label} must be a positive integer`);
  }
  return value;
}

function requireString(value, label) {
  if (typeof value !== "string" || !value.trim()) {
    throw new Error(`${label} must be a non-empty string`);
  }
  return value.trim();
}

function optionalString(value, label) {
  if (value == null) {
    return null;
  }
  return requireString(value, label);
}

function optionalBoolean(value, fallback, label) {
  if (value == null) {
    return fallback;
  }
  if (typeof value !== "boolean") {
    throw new Error(`${label} must be a boolean`);
  }
  return value;
}

function optionalPositiveInteger(value, label) {
  if (value == null) {
    return null;
  }
  return requirePositiveInteger(value, label);
}

const ANDROID_KEY_MODIFIER_ALIASES = new Map([
  ["alt", "ALT_LEFT"],
  ["alt_left", "ALT_LEFT"],
  ["altleft", "ALT_LEFT"],
  ["control", "CTRL_LEFT"],
  ["ctrl", "CTRL_LEFT"],
  ["ctrl_left", "CTRL_LEFT"],
  ["ctrlleft", "CTRL_LEFT"],
  ["meta", "META_LEFT"],
  ["meta_left", "META_LEFT"],
  ["metaleft", "META_LEFT"],
  ["shift", "SHIFT_LEFT"],
  ["shift_left", "SHIFT_LEFT"],
  ["shiftleft", "SHIFT_LEFT"],
]);

const ANDROID_KEY_MODIFIER_NAMES = new Set(ANDROID_KEY_MODIFIER_ALIASES.values());

function normalizeAndroidCombinationKey(key) {
  const value = requireString(key, "keycode");
  const alias = ANDROID_KEY_MODIFIER_ALIASES.get(value.toLowerCase().replace(/[\s-]+/g, "_"));
  if (alias) {
    return alias;
  }
  return value.startsWith("KEYCODE_") ? value.slice("KEYCODE_".length) : value.toUpperCase();
}

function shouldDispatchAsKeyCombination(keys) {
  return keys.length > 1 && keys.some((key) => {
    const normalized = normalizeAndroidCombinationKey(key);
    return ANDROID_KEY_MODIFIER_NAMES.has(normalized);
  });
}

function optionalNumber(value) {
  return typeof value === "number" && !Number.isNaN(value) ? value : null;
}

function readNumber(value, ...paths) {
  for (const pathEntry of paths) {
    const keys = Array.isArray(pathEntry) ? pathEntry : [pathEntry];
    let current = value;
    let valid = true;
    for (const key of keys) {
      if (!current || typeof current !== "object" || Array.isArray(current)) {
        valid = false;
        break;
      }
      current = current[key];
    }
    if (valid) {
      const number = optionalNumber(current);
      if (number != null) {
        return number;
      }
    }
  }
  return null;
}

function normalizeStepActionType(type, label = "action type") {
  const normalized = requireString(type, label);
  switch (normalized) {
    case "tap":
      return "click";
    case "type_text":
      return "type";
    case "key":
      return "keypress";
    case "swipe":
      return "drag";
    default:
      return normalized;
  }
}

function normalizeActionShape(action, label) {
  const source = requireObject(action, label);
  const normalized = { ...source };
  normalized.type = normalizeStepActionType(source.type, `${label}.type`);
  if (normalized.selector == null && normalized.target != null) {
    normalized.selector = normalized.target;
  }
  if (normalized.type === "keypress") {
    if (!Array.isArray(normalized.keys)) {
      const key = normalized.key ?? normalized.keycode;
      normalized.keys = typeof key === "string" ? [key] : [];
    }
  }
  if (normalized.type === "wait" && optionalNumber(normalized.ms) == null) {
    normalized.ms =
      optionalNumber(normalized.wait_ms) ??
      optionalNumber(normalized.timeout_ms) ??
      1000;
  }
  return normalized;
}

function normalizeLegacyActionArgs(args) {
  const normalized = {
    ...requireObject(args, "arguments"),
    type: normalizeStepActionType(args.action, "arguments.action"),
  };
  if (normalized.selector == null && normalized.target != null) {
    normalized.selector = normalized.target;
  }
  if (normalized.type === "keypress") {
    const key = normalized.key ?? normalized.keycode;
    normalized.keys = typeof key === "string" ? [key] : [];
  }
  if (normalized.type === "wait") {
    normalized.ms =
      optionalNumber(normalized.wait_ms) ??
      optionalNumber(normalized.timeout_ms) ??
      1000;
  }
  return normalized;
}

function normalizeStepActions(args) {
  if (Array.isArray(args?.actions)) {
    if (args.actions.length === 0) {
      throw new Error("android_step actions[] must not be empty");
    }
    return args.actions.map((action, index) =>
      normalizeActionShape(action, `actions[${index}]`),
    );
  }

  if (args?.action != null) {
    return [normalizeLegacyActionArgs(args)];
  }

  throw new Error("android_step requires either actions[] or action");
}

function normalizeMultiTouchPointers(value) {
  if (!Array.isArray(value) || value.length < 2 || value.length > 5) {
    throw new Error("multi_touch pointers must contain 2 to 5 paths");
  }
  return value.map((pointer, index) => {
    const source = requireObject(pointer, `pointers[${index}]`);
    return Object.fromEntries(
      ["x1", "y1", "x2", "y2"].map((field) => {
        const coordinate = requireNumber(source[field], `pointers[${index}].${field}`);
        if (!Number.isSafeInteger(coordinate) || coordinate < 0) {
          throw new Error(`pointers[${index}].${field} must be a non-negative integer`);
        }
        return [field, coordinate];
      }),
    );
  });
}

function normalizeMultiTouchDuration(value) {
  const durationMs = value == null ? 300 : requireNumber(value, "duration_ms");
  if (!Number.isSafeInteger(durationMs) || durationMs < 50 || durationMs > 2000) {
    throw new Error("multi_touch duration_ms must be an integer from 50 through 2000");
  }
  return durationMs;
}

function normalizeInstallBuildFromRunArgs(args) {
  const source = requireObject(args, "arguments");
  const contractVersion = optionalString(
    source.contract_version ?? source.contractVersion,
    "contract_version",
  );
  const target = source.target == null ? null : requireObject(source.target, "target");
  const install = source.install == null ? null : requireObject(source.install, "install");
  if (install && !contractVersion) {
    throw new Error("install requires contract_version");
  }
  const normalized = {
    workflow_run_id: requirePositiveInteger(
      source.workflow_run_id ?? source.workflowRunId,
      "workflow_run_id",
    ),
    artifact_name: requireString(
      source.artifact_name ?? source.artifactName,
      "artifact_name",
    ),
    repository: optionalString(source.repository, "repository"),
    serial: optionalString(source.serial, "serial"),
    timeout_secs: optionalPositiveInteger(
      source.timeout_secs ?? source.timeoutSecs,
      "timeout_secs",
    ),
  };
  if (!contractVersion) {
    return {
      ...normalized,
      launch_after_install: optionalBoolean(
        source.launch_after_install ?? source.launchAfterInstall,
        true,
        "launch_after_install",
      ),
    };
  }
  if (!install || typeof install.launch_after_install !== "boolean") {
    throw new Error("native install requires install.launch_after_install as a boolean");
  }
  return {
    ...normalized,
    contract_version: contractVersion,
    target,
    install: {
      launch_after_install: install.launch_after_install,
    },
  };
}

function normalizeView(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return null;
  }

  const deviceWidth =
    readNumber(value, "deviceWidth", "device_width", ["device", "width"]);
  const deviceHeight =
    readNumber(value, "deviceHeight", "device_height", ["device", "height"]);
  if (deviceWidth == null || deviceHeight == null) {
    return null;
  }

  const frameWidth =
    readNumber(value, "frameWidth", "frame_width", ["frame", "width"]) ??
    deviceWidth;
  const frameHeight =
    readNumber(value, "frameHeight", "frame_height", ["frame", "height"]) ??
    deviceHeight;

  const region = value.region && typeof value.region === "object" && !Array.isArray(value.region)
    ? {
        left: optionalNumber(value.region.left) ?? 0,
        top: optionalNumber(value.region.top) ?? 0,
        width: optionalNumber(value.region.width) ?? deviceWidth,
        height: optionalNumber(value.region.height) ?? deviceHeight,
      }
    : null;

  return {
    deviceWidth,
    deviceHeight,
    frameWidth,
    frameHeight,
    region,
  };
}

function summarizeView(view) {
  if (!view) {
    return null;
  }
  return JSON.stringify({
    deviceWidth: view.device?.width ?? null,
    deviceHeight: view.device?.height ?? null,
    frameWidth: view.frame?.width ?? null,
    frameHeight: view.frame?.height ?? null,
    region: view.region ?? null,
    zoomed: view.zoomed ?? false,
  });
}

function compactSummaryText(text, limit = 96) {
  if (typeof text !== "string" || text.length <= limit) {
    return text;
  }
  return `${text.slice(0, limit - 3)}...`;
}

function buildScrollSwipe(action, { frameWidth = null, frameHeight = null } = {}) {
  const anchorX = optionalNumber(action.x) ?? (frameWidth != null ? frameWidth / 2 : 500);
  const anchorY = optionalNumber(action.y) ?? (frameHeight != null ? frameHeight / 2 : 900);
  const travel = Math.max(
    Math.abs(optionalNumber(action.scroll_y) ?? 0),
    Math.abs(optionalNumber(action.scroll_x) ?? 0),
    120,
  );

  if (Math.abs(optionalNumber(action.scroll_y) ?? 0) >= Math.abs(optionalNumber(action.scroll_x) ?? 0)) {
    const direction = (optionalNumber(action.scroll_y) ?? 0) >= 0 ? -1 : 1;
    return {
      x1: Math.round(anchorX),
      y1: Math.round(anchorY),
      x2: Math.round(anchorX),
      y2: Math.round(anchorY + direction * travel),
    };
  }

  const direction = (optionalNumber(action.scroll_x) ?? 0) >= 0 ? -1 : 1;
  return {
    x1: Math.round(anchorX),
    y1: Math.round(anchorY),
    x2: Math.round(anchorX + direction * travel),
    y2: Math.round(anchorY),
  };
}

function bridgeReadyAction(action) {
  if (!BRIDGE_BATCH_ACTION_VALUES.has(action.type)) {
    return null;
  }
  const tapOptions = {
    timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
    waitForSelector: action.wait_for_selector ?? null,
    waitUntilAbsent: action.wait_until_absent === true,
    matchIndex: action.match_index ?? null,
  };
  const typeOptions = {
    timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
    waitForSelector: action.wait_for_selector ?? null,
    expectFocusSelector: action.expect_focus_selector ?? null,
  };
  switch (action.type) {
    case "click":
      return action.selector == null
        ? {
            ...action,
            tapOptions,
          }
        : null;
    case "double_click":
    case "long_press":
      return {
        ...action,
        tapOptions,
      };
    case "type":
      return action.selector == null
        ? {
            ...action,
            typeOptions,
          }
        : null;
    case "keypress":
      return {
        ...action,
        typeOptions: {
          timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
          waitForSelector: action.wait_for_selector ?? null,
          expectFocusSelector: action.expect_focus_selector ?? null,
        },
        keyOptions: {
          timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
          waitForSelector: action.wait_for_selector ?? null,
          waitForActivity: action.wait_for_activity ?? null,
          waitForPackage: action.wait_for_package ?? null,
        },
      };
    case "drag":
      return {
        ...action,
        duration_ms: typeof action.duration_ms === "number" ? action.duration_ms : undefined,
        wait_for_selector: action.wait_for_selector ?? null,
        expect_scroll_change: action.expect_scroll_change === true,
        timeout_secs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
      };
    case "scroll":
      return {
        ...action,
        timeout_secs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
      };
    default:
      return action;
  }
}

function deriveFocusSelectorForTypedAction(action) {
  if (action?.expect_focus_selector != null) {
    return requireObject(action.expect_focus_selector, "expect_focus_selector");
  }

  if (!action?.selector || typeof action.selector !== "object" || Array.isArray(action.selector)) {
    return null;
  }

  const derived = { ...action.selector, focused: true };
  delete derived.text;
  delete derived.text_exact;
  delete derived.label;
  delete derived.label_exact;

  if (Object.keys(derived).length === 1 && derived.focused === true) {
    derived.focusable = true;
  }

  return derived;
}

function normalizeSelectorAliases(selector) {
  if (!selector || typeof selector !== "object" || Array.isArray(selector)) {
    return selector;
  }
  const normalized = { ...selector };
  const contentDesc =
    normalized.content_desc ?? normalized.content_description ?? normalized.contentDescription;
  const resourceId = normalized.resource_id ?? normalized.resourceId;
  const className = normalized.class_name ?? normalized.className;
  if (contentDesc != null) {
    normalized.content_desc = contentDesc;
  } else {
    delete normalized.content_desc;
  }
  if (resourceId != null) {
    normalized.resource_id = resourceId;
  } else {
    delete normalized.resource_id;
  }
  if (className != null) {
    normalized.class_name = className;
  } else {
    delete normalized.class_name;
  }
  delete normalized.content_description;
  delete normalized.contentDescription;
  delete normalized.resourceId;
  delete normalized.className;
  return normalized;
}

function selectorBoundsCenter(selector) {
  const bounds = selector?.bounds;
  if (!bounds || typeof bounds !== "object" || Array.isArray(bounds)) {
    return null;
  }
  const left = optionalNumber(bounds.left);
  const top = optionalNumber(bounds.top);
  const right = optionalNumber(bounds.right);
  const bottom = optionalNumber(bounds.bottom);
  if (
    left == null ||
    top == null ||
    right == null ||
    bottom == null ||
    right <= left ||
    bottom <= top
  ) {
    throw new Error("selector.bounds must define a positive left/top/right/bottom rectangle");
  }
  return {
    x: Math.round((left + right) / 2),
    y: Math.round((top + bottom) / 2),
  };
}

function selectorWithoutBounds(selector) {
  if (!selector || typeof selector !== "object" || Array.isArray(selector)) {
    return selector;
  }
  const normalized = { ...selector };
  delete normalized.bounds;
  return normalized;
}

function normalizeTapSelector(selector) {
  if (typeof selector !== "string") {
    return normalizeSelectorAliases(selector);
  }
  const trimmed = selector.trim();
  if (!trimmed) {
    return selector;
  }
  // Bare string tap selectors should target the interactive control labelled
  // with that text, not any arbitrary text node that happens to contain it.
  return {
    label: trimmed,
    label_exact: true,
  };
}

async function completeAbsentSelectorWait(runtime, tapResult, waitForSelector, timeoutSecs) {
  if (waitForSelector == null) {
    return tapResult;
  }
  if (typeof runtime.waitForUiElement !== "function") {
    throw new Error("runtime.waitForUiElement is unavailable for an absent-selector postcondition");
  }
  const waitResult = await runtime.waitForUiElement(waitForSelector, {
    timeoutSecs,
    absent: true,
  });
  return {
    ok: Boolean(tapResult?.ok ?? true) && Boolean(waitResult?.ok ?? true),
    postcondition: waitResult?.postcondition ?? waitResult,
    result: {
      tap: tapResult,
      wait: waitResult,
    },
  };
}

function stepSummaryLines({
  actions,
  actionResults,
  context,
  uiDigest,
  observeWarning,
  view,
  outcome,
}) {
  const lines = ["Android step"];
  lines.push(`actions executed: ${actionResults.length}`);
  lines.push(`actions: ${actions.map((action) => action.type).join(" -> ")}`);
  if (outcome?.status) {
    lines.push(`outcome: ${outcome.status}`);
  }
  if (outcome?.retryability) {
    lines.push(`retryability: ${outcome.retryability}`);
  }
  if (actions.length === 1 && actions[0]?.selector != null) {
    lines.push(`selector: ${compactJson(actions[0].selector)}`);
  }
  if (actions.length === 1 && typeof actions[0]?.text === "string" && actions[0].text.trim()) {
    lines.push(`text: ${actions[0].text.trim()}`);
  }
  const primaryResult =
    actionResults.find((result) => typeof result?.ok === "boolean") ?? actionResults[0] ?? null;
  if (typeof primaryResult?.ok === "boolean") {
    lines.push(`ok: ${primaryResult.ok}`);
  }
  if (typeof primaryResult?.postcondition?.satisfied === "boolean") {
    lines.push(`postcondition satisfied: ${primaryResult.postcondition.satisfied}`);
  }
  if (primaryResult?.observed_activity) {
    lines.push(`activity: ${primaryResult.observed_activity}`);
  }
  if (primaryResult?.observed_package) {
    lines.push(`package: ${primaryResult.observed_package}`);
  }
  if (context?.serial) {
    lines.push(`serial: ${context.serial}`);
  }
  const firstNote = actionResults.find((result) => typeof result?.note === "string");
  if (firstNote) {
    lines.push(`note: ${firstNote.note}`);
  }
  if (observeWarning) {
    lines.push(observeWarning);
  }
  if (uiDigest) {
    lines.push(uiDigest);
  }
  const summarizedView = summarizeView(view);
  if (summarizedView) {
    lines.push(`view: ${summarizedView}`);
  }
  return lines;
}

function installSummaryLines({ args, result, context, uiDigest, observeWarning, outcome }) {
  const lines = ["Android build install"];
  if (outcome?.status) {
    lines.push(`outcome: ${outcome.status}`);
  }
  if (outcome?.retryability) {
    lines.push(`retryability: ${outcome.retryability}`);
  }
  lines.push(`workflow_run_id: ${args.workflow_run_id}`);
  lines.push(`artifact_name: ${args.artifact_name}`);
  if (args.repository) {
    lines.push(`repository: ${args.repository}`);
  }
  for (const field of [
    "ok",
    "installed",
    "reused_existing_build",
    "uninstalled_existing_package",
  ]) {
    if (typeof result?.[field] === "boolean") {
      lines.push(`${field}: ${result[field]}`);
    }
  }
  if (result?.serial) {
    lines.push(`serial: ${result.serial}`);
  } else if (context?.serial) {
    lines.push(`serial: ${context.serial}`);
  }
  const manifest = result?.manifest;
  if (manifest && typeof manifest === "object") {
    for (const field of [
      "run_id",
      "checkout_ref",
      "commit_sha",
      "version_name",
      "package_name",
      "activity_name",
      "android_validation_mode",
      "interactive_debug_profile",
    ]) {
      if (typeof manifest[field] === "string" && manifest[field].trim()) {
        lines.push(`${field}: ${compactSummaryText(manifest[field])}`);
      }
    }
  }
  if (typeof result?.postcondition?.satisfied === "boolean") {
    lines.push(`postcondition satisfied: ${result.postcondition.satisfied}`);
  }
  if (observeWarning) {
    lines.push(observeWarning);
  }
  if (uiDigest) {
    lines.push(uiDigest);
  }
  return lines;
}

function observationSummaryLines({
  prompt,
  scope,
  context,
  uiDigest,
  observeWarning,
  outcome,
}) {
  const lines = ["Android observation"];
  if (outcome?.status) {
    lines.push(`outcome: ${outcome.status}`);
  }
  if (outcome?.retryability) {
    lines.push(`retryability: ${outcome.retryability}`);
  }
  if (prompt) {
    lines.push(`focus: ${prompt}`);
  }
  lines.push(`scope: ${scope}`);
  if (context?.serial) {
    lines.push(`serial: ${context.serial}`);
  }
  if (context?.windowState?.input_method_visible === true) {
    const target = context.windowState.input_method_target;
    lines.push(target ? `soft_keyboard: visible for ${target}` : "soft_keyboard: visible");
  }
  if (observeWarning) {
    lines.push(observeWarning);
  }
  if (uiDigest) {
    lines.push(uiDigest);
  }
  return lines;
}

function failureSummaryLines({ tool, outcome }) {
  const label = {
    [TOOL_ANDROID_OBSERVE]: "observation",
    [TOOL_ANDROID_STEP]: "step",
    [TOOL_ANDROID_INSTALL_BUILD_FROM_RUN]: "build install",
  }[tool] ?? "step";
  const lines = [`Android ${label} failed`];
  lines.push(`outcome: ${outcome.status}`);
  lines.push(`retryability: ${outcome.retryability}`);
  if (outcome.actionTypes.length > 0) {
    lines.push(`actions: ${outcome.actionTypes.join(" -> ")}`);
  }
  if (outcome.reason) {
    lines.push(`reason: ${outcome.reason}`);
  }
  return lines;
}

export function createCodexAndroidDynamicToolHost({
  runtime,
  readFile = fs.readFile,
  readRemoteArtifactFile = null,
  defaultPackageName = null,
  defaultActivity = null,
} = {}) {
  if (!runtime || typeof runtime !== "object") {
    throw new Error("createCodexAndroidDynamicToolHost requires a runtime");
  }
  async function readArtifactFile(filePath, encoding = undefined) {
    if (
      typeof readRemoteArtifactFile === "function" &&
      shouldPreferRemoteArtifactRead(filePath)
    ) {
      return readRemoteArtifactFile(filePath, encoding);
    }

    try {
      return await readFile(filePath, encoding);
    } catch (error) {
      if (
        typeof readRemoteArtifactFile === "function" &&
        isMissingLocalArtifactError(error)
      ) {
        return readRemoteArtifactFile(filePath, encoding);
      }
      throw error;
    }
  }

  const responsesItems = createAndroidResponsesItemAdapter({
    readFile: readArtifactFile,
  });

  function createOutcome({
    tool,
    status,
    retryability = "none",
    actionTypes = [],
    reason = null,
    postconditionSatisfied = null,
    observeDegraded = false,
  }) {
    return {
      tool,
      status,
      retryability,
      actionTypes,
      reason,
      postconditionSatisfied,
      observeDegraded,
    };
  }

  function errorMessage(error) {
    return error instanceof Error ? error.message : String(error);
  }

  function safeActionTypes(args) {
    try {
      return normalizeStepActions(args).map((action) => action.type);
    } catch {
      return [];
    }
  }

  function isKnownTool(tool) {
    return tool === TOOL_ANDROID_OBSERVE ||
      tool === TOOL_ANDROID_STEP ||
      tool === TOOL_ANDROID_INSTALL_BUILD_FROM_RUN;
  }

  function outcomeFromError({ tool, args, error }) {
    const message = errorMessage(error);
    const result = error?.result;
    const actionTypes = tool === TOOL_ANDROID_STEP ? safeActionTypes(args) : [];

    if (
      (tool === TOOL_ANDROID_STEP || tool === TOOL_ANDROID_INSTALL_BUILD_FROM_RUN) &&
      (
        result?.ok === false ||
        result?.postcondition?.satisfied === false ||
        /did not report success|postcondition|outcome was not satisfied/i.test(message)
      )
    ) {
      return createOutcome({
        tool,
        status: "postcondition_failed",
        retryability: "observe_then_retry",
        actionTypes,
        reason: message,
        postconditionSatisfied: result?.postcondition?.satisfied ?? false,
      });
    }

    if (/stale android_step\.view geometry|cannot safely use stale/i.test(message)) {
      return createOutcome({
        tool,
        status: "stale_view",
        retryability: "observe_then_retry",
        actionTypes,
        reason: message,
      });
    }

    if (/unsupported capability|operator action is required/i.test(message)) {
      return createOutcome({
        tool,
        status: "unsupported_capability",
        retryability: "operator_required",
        actionTypes,
        reason: message,
      });
    }

    if (
      /requires|must be|must not|unsupported android_step action|action must be one of|unknown argument/i
        .test(message)
    ) {
      return createOutcome({
        tool,
        status: "invalid_request",
        retryability: "none",
        actionTypes,
        reason: message,
      });
    }

    if (/no android device serial|unavailable|did not include|mcp|connection|timeout|fetch failed|initialize failed/i.test(message)) {
      const retryability =
        /cloudflare tunnel error|error 1033|tunnel_error|retryable|temporarily|timeout|connection|fetch failed|econnreset|econnrefused|enotfound|50[234]|530/i
          .test(message)
          ? "retry_same_request"
          : "operator_required";
      return createOutcome({
        tool,
        status: "provider_unavailable",
        retryability,
        actionTypes,
        reason: message,
      });
    }

    return createOutcome({
      tool,
      status: "failed",
      retryability: "operator_required",
      actionTypes,
      reason: message,
    });
  }

  function createFailureResponse({ tool, args, error }) {
    const outcome = outcomeFromError({ tool, args, error });
    return createDynamicToolCallResponse(
      [
        {
          type: "inputText",
          text: failureSummaryLines({ tool, outcome }).join("\n"),
        },
      ],
      {
        success: false,
        metadata: {
          android: {
            outcome,
          },
        },
      },
    );
  }

  function outcomeFromObservation({ observeWarning }) {
    return createOutcome({
      tool: TOOL_ANDROID_OBSERVE,
      status: observeWarning ? "observe_degraded" : "succeeded",
      retryability: observeWarning ? "observe_then_retry" : "none",
      reason: observeWarning ?? null,
      observeDegraded: Boolean(observeWarning),
    });
  }

  function outcomeFromStep({ actions, actionResults, observeWarning }) {
    const normalizedResults = actionResults.map((result) => {
      const wrappedPayload =
        result && typeof result.result === "object" && result.result != null
          ? result.result
          : null;
      const ok =
        typeof result?.ok === "boolean"
          ? result.ok
          : typeof wrappedPayload?.ok === "boolean"
            ? wrappedPayload.ok
            : true;
      const postconditionSatisfied =
        typeof result?.postcondition?.satisfied === "boolean"
          ? result.postcondition.satisfied
          : typeof wrappedPayload?.postcondition?.satisfied === "boolean"
            ? wrappedPayload.postcondition.satisfied
            : null;
      return {
        original: result,
        payload: wrappedPayload ?? result,
        ok,
        postconditionSatisfied,
      };
    });
    const failedResult = normalizedResults.find((result) =>
      !result.ok || result.postconditionSatisfied === false,
    ) ?? null;

    if (failedResult) {
      return createOutcome({
        tool: TOOL_ANDROID_STEP,
        status: "postcondition_failed",
        retryability: "observe_then_retry",
        actionTypes: actions.map((action) => action.type),
        reason: failedResult.payload?.note ?? "Android action completed but the requested outcome was not satisfied",
        postconditionSatisfied: failedResult.postconditionSatisfied,
        observeDegraded: Boolean(observeWarning),
      });
    }

    const firstPostcondition = normalizedResults.find((result) =>
      typeof result.postconditionSatisfied === "boolean",
    )?.postconditionSatisfied ?? null;

    return createOutcome({
      tool: TOOL_ANDROID_STEP,
      status: observeWarning ? "observe_degraded" : "succeeded",
      retryability: observeWarning ? "observe_then_retry" : "none",
      actionTypes: actions.map((action) => action.type),
      reason: observeWarning ?? null,
      postconditionSatisfied: firstPostcondition,
      observeDegraded: Boolean(observeWarning),
    });
  }

  function outcomeFromInstall({ result, observeWarning }) {
    if (result?.ok === false || result?.postcondition?.satisfied === false) {
      const providerRetryability = result?.error?.retryability;
      return createOutcome({
        tool: TOOL_ANDROID_INSTALL_BUILD_FROM_RUN,
        status: "postcondition_failed",
        retryability: providerRetryability === "do_not_replay"
          ? "do_not_replay"
          : "observe_then_retry",
        reason: result?.error?.message
          ?? result?.postcondition?.note
          ?? "Android build install completed but the requested outcome was not satisfied",
        postconditionSatisfied: result?.postcondition?.satisfied ?? false,
        observeDegraded: Boolean(observeWarning),
      });
    }

    return createOutcome({
      tool: TOOL_ANDROID_INSTALL_BUILD_FROM_RUN,
      status: observeWarning ? "observe_degraded" : "succeeded",
      retryability: observeWarning ? "observe_then_retry" : "none",
      reason: observeWarning ?? null,
      postconditionSatisfied: result?.postcondition?.satisfied ?? null,
      observeDegraded: Boolean(observeWarning),
    });
  }

  async function ensureSerial() {
    if (runtime.getState().currentSerial) {
      return runtime.getState().currentSerial;
    }
    const devices = await runtime.listDevices();
    const serial = devices?.devices?.[0]?.serial ?? null;
    if (!serial) {
      throw new Error("No Android device serial is available for the Codex dynamic tool host");
    }
    await runtime.setSerial(serial);
    return serial;
  }

  async function visualContextContentItems(context, { summaryLines }) {
    const contentItems = [
      {
        type: "inputText",
        text: summaryLines.join("\n"),
      },
    ];

    if (context?.screenshotPath) {
      const imageItem = await responsesItems.inputImageFromPath(context.screenshotPath, {
        detail: "original",
      });
      contentItems.push(toDynamicToolContentItem(imageItem));
    }

    return contentItems;
  }

  function nativeImageWarning(context, phase) {
    if (context?.screenshotPath) {
      return null;
    }
    return `${phase} missing native image output: screenshot capture did not return an artifact path`;
  }

  function combineObserveWarnings(...warnings) {
    const present = warnings.filter((warning) =>
      typeof warning === "string" && warning.trim(),
    );
    return present.length > 0 ? present.join("; ") : null;
  }

  async function captureObserveContext({
    scope,
    actionLabel,
    screenshotLabel,
    hierarchyFilename,
    screenshotFilename,
  }) {
    if (scope === "screen") {
      const screenshot = await runtime.captureScreenshot(screenshotLabel);
      return {
        context: {
          serial: runtime.getState().currentSerial,
          screenshotPath: screenshot?.path ?? null,
          uiDumpPath: null,
          lastScenarioResult: null,
        },
        observeWarning: null,
      };
    }

    try {
      const observation = await runtime.waitForStableUi({
        hierarchyFilename,
        screenshotFilename,
      });
      return {
        context: contextFromObservation(observation),
        observeWarning: null,
      };
    } catch (error) {
      const screenshot = await runtime.captureScreenshot(screenshotLabel);
      return {
        context: {
          serial: runtime.getState().currentSerial,
          screenshotPath: screenshot?.path ?? null,
          uiDumpPath: null,
          lastScenarioResult: null,
        },
        observeWarning: `ui digest unavailable after ${actionLabel}: ${summarizeObserveError(error)}`,
      };
    }
  }

  async function observeAfterAction(action, scope) {
    return captureObserveContext({
      scope,
      actionLabel: action,
      screenshotLabel: `codex-${action}-after`,
      hierarchyFilename: `codex-${action}-after.xml`,
      screenshotFilename: `codex-${action}-after.png`,
    });
  }

  function createComputerBridge(args) {
    const normalizedView = normalizeView(args?.view);
    if (!normalizedView) {
      return null;
    }

    const bridge = createAndroidComputerBridge({
      runtime,
      deviceWidth: normalizedView.deviceWidth,
      deviceHeight: normalizedView.deviceHeight,
      frameWidth: normalizedView.frameWidth,
      frameHeight: normalizedView.frameHeight,
    });
    if (normalizedView.region) {
      bridge.setZoomRegion(normalizedView.region, {
        width: normalizedView.frameWidth,
        height: normalizedView.frameHeight,
      });
    }
    return bridge;
  }

  async function observeAfterStep(actions, scope, bridge) {
    const actionLabel =
      actions.length === 1
        ? actions[0].type
        : `batch-${actions.length}`;
    if (bridge) {
      const includeUi = scope === "screen_and_ui";
      try {
        const captured = await bridge.captureView(`codex-${actionLabel}-after`, {
          includeUi,
          display: false,
        });
        return {
          context: {
            serial: captured.serial ?? runtime.getState().currentSerial,
            screenshotPath: captured.screenshotPath ?? null,
            uiDumpPath: captured.uiDumpPath ?? null,
            lastScenarioResult: null,
          },
          observeWarning: null,
          view: captured.view ?? bridge.getView(),
        };
      } catch (error) {
        const observed = await observeAfterAction(actionLabel, scope);
        return {
          ...observed,
          observeWarning: `view-aware capture unavailable after ${actionLabel}: ${summarizeObserveError(error)}`,
          view: bridge.getView(),
        };
      }
    }

    const observed = await observeAfterAction(actionLabel, scope);
    return {
      ...observed,
      view: null,
    };
  }

  async function handleObserve(args) {
    await ensureSerial();

    const scope = OBSERVE_SCOPE_VALUES.includes(args?.scope)
      ? args.scope
      : "screen_and_ui";
    const prompt = typeof args?.prompt === "string" && args.prompt.trim()
      ? args.prompt.trim()
      : null;

    const { context, observeWarning } = await captureObserveContext({
      scope,
      actionLabel: "observe",
      screenshotLabel: "codex-android-observe",
      hierarchyFilename: "codex-android-observe.xml",
      screenshotFilename: "codex-android-observe.png",
    });

    const uiDigest =
      scope === "screen_and_ui"
        && context.uiDumpPath
        ? await summarizeUiDump(context.uiDumpPath, readArtifactFile)
        : null;
    const visualWarning = combineObserveWarnings(
      observeWarning,
      nativeImageWarning(context, "android_observe"),
    );
    const outcome = outcomeFromObservation({ observeWarning: visualWarning });
    const hasNativeImage = Boolean(context?.screenshotPath);

    return createDynamicToolCallResponse(
      await visualContextContentItems(context, {
        summaryLines: observationSummaryLines({
          prompt,
          scope,
          context,
          uiDigest,
          observeWarning: visualWarning,
          outcome,
        }),
      }),
      {
        success: hasNativeImage,
        metadata: {
          android: {
            outcome,
          },
        },
      },
    );
  }

  async function performStepAction(action, bridge, stepDefaults) {
    const bridgeAction = bridgeReadyAction(action);
    if (bridge && bridgeAction) {
      const batch = await bridge.executeActionBatch([bridgeAction], {
        captureAfter: false,
      });
      return batch.results[0];
    }

    switch (action.type) {
      case "launch_app":
      {
        const packageName =
          action.package_name ??
          action.package ??
          stepDefaults.defaultPackageName ??
          requireString(action.package_name ?? action.package, "package_name");
        const defaultActivity =
          packageName === stepDefaults.defaultPackageName
            ? stepDefaults.defaultActivity
            : null;
        return runtime.launchApp(
          packageName,
          {
            activity: action.activity ?? defaultActivity ?? null,
            waitForSelector: action.wait_for_selector ?? null,
            waitForActivity: action.wait_for_activity ?? null,
            waitForPackage: action.wait_for_package ?? null,
            timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
          },
        );
      }
      case "click":
        if (action.selector != null) {
          const selector = normalizeTapSelector(action.selector);
          const waitForSelector = normalizeSelectorAliases(action.wait_for_selector ?? null);
          const center = selectorBoundsCenter(selector);
          const tapResult = center
            ? await runtime.tap(center.x, center.y, {
              timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
              waitForSelector: action.wait_until_absent === true ? null : waitForSelector,
              waitUntilAbsent: action.wait_until_absent === true && waitForSelector == null,
              tappedSelector: null,
            })
            : await runtime.tapElement(selectorWithoutBounds(selector), {
              timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
              waitForSelector: action.wait_until_absent === true ? null : waitForSelector,
              waitUntilAbsent: action.wait_until_absent === true && waitForSelector == null,
              matchIndex: action.match_index ?? null,
            });
          if (action.wait_until_absent !== true || waitForSelector == null) {
            return tapResult;
          }
          return completeAbsentSelectorWait(
            runtime,
            tapResult,
            waitForSelector,
            typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
          );
        }
        const coordinateWaitForSelector = normalizeSelectorAliases(
          action.wait_for_selector ?? null,
        );
        const coordinateTapResult = await runtime.tap(
          requireNumber(action.x, "x"),
          requireNumber(action.y, "y"),
          {
            timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
            waitForSelector:
              action.wait_until_absent === true ? null : coordinateWaitForSelector,
            waitUntilAbsent:
              action.wait_until_absent === true && coordinateWaitForSelector == null,
          },
        );
        if (action.wait_until_absent !== true || coordinateWaitForSelector == null) {
          return coordinateTapResult;
        }
        return completeAbsentSelectorWait(
          runtime,
          coordinateTapResult,
          coordinateWaitForSelector,
          typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
        );
      case "double_click": {
        const x = requireNumber(action.x, "x");
        const y = requireNumber(action.y, "y");
        const options = {
          timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
          waitForSelector: action.wait_for_selector ?? null,
        };
        if (typeof runtime.doubleTap === "function") {
          return runtime.doubleTap(x, y, options);
        }
        const first = await runtime.tap(x, y, options);
        const second = await runtime.tap(x, y, options);
        return {
          ok: Boolean(first?.ok ?? true) && Boolean(second?.ok ?? true),
          result: {
            first,
            second,
            fallback: "runtime.doubleTap unavailable; dispatched as two tap actions",
          },
        };
      }
      case "long_press":
        return runtime.longPress(
          requireNumber(action.x, "x"),
          requireNumber(action.y, "y"),
          {
            durationMs: typeof action.duration_ms === "number" ? action.duration_ms : 500,
            timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
            waitForSelector: action.wait_for_selector ?? null,
          },
        );
      case "type":
        if (action.selector != null) {
          const selector = normalizeSelectorAliases(action.selector);
          const center = selectorBoundsCenter(selector);
          if (center == null && typeof runtime.typeIntoElement === "function") {
            return runtime.typeIntoElement(
              selectorWithoutBounds(selector),
              requireString(action.text, "text"),
              {
                timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
                matchIndex: action.match_index ?? null,
              },
            );
          }
          const tapResult = center
            ? await runtime.tap(center.x, center.y, {
              timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
            })
            : await runtime.tapElement(selectorWithoutBounds(selector), {
              timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
              matchIndex: action.match_index ?? null,
            });
          const typeResult = await runtime.typeText(requireString(action.text, "text"), {
            timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
            waitForSelector: normalizeSelectorAliases(action.wait_for_selector ?? null),
            expectFocusSelector: deriveFocusSelectorForTypedAction(action),
          });
          return {
            ok: Boolean(tapResult?.ok ?? true) && Boolean(typeResult?.ok ?? true),
            postcondition: typeResult?.postcondition ?? tapResult?.postcondition ?? null,
            result: {
              tap: tapResult,
              type: typeResult,
            },
          };
        }
        return runtime.typeText(requireString(action.text, "text"), {
          timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
          waitForSelector: action.wait_for_selector ?? null,
          expectFocusSelector: action.expect_focus_selector ?? null,
        });
      case "keypress": {
        const keys = Array.isArray(action.keys) ? action.keys : [];
        if (shouldDispatchAsKeyCombination(keys) && typeof runtime.keyCombination === "function") {
          return runtime.keyCombination(keys.map(normalizeAndroidCombinationKey), {
            timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
            waitForSelector: action.wait_for_selector ?? null,
            waitForActivity: action.wait_for_activity ?? null,
            waitForPackage: action.wait_for_package ?? null,
          });
        }
        const results = [];
        for (const key of keys) {
          if (key.length === 1 && /[ -~]/.test(key)) {
            results.push(
              await runtime.typeText(key, {
                timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
              }),
            );
          } else {
            results.push(
              await runtime.keyevent(requireString(key, "keycode"), {
                timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
                waitForSelector: action.wait_for_selector ?? null,
                waitForActivity: action.wait_for_activity ?? null,
                waitForPackage: action.wait_for_package ?? null,
              }),
            );
          }
        }
        return {
          ok: results.every((result) => result?.ok !== false),
          result: results,
        };
      }
      case "drag":
        return runtime.swipe(
          requireNumber(action.x1, "x1"),
          requireNumber(action.y1, "y1"),
          requireNumber(action.x2, "x2"),
          requireNumber(action.y2, "y2"),
          {
            durationMs: typeof action.duration_ms === "number" ? action.duration_ms : undefined,
            timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
            waitForSelector: action.wait_for_selector ?? null,
            expectScrollChange: action.expect_scroll_change === true,
          },
        );
      case "multi_touch":
        if (typeof runtime.multiTouch !== "function") {
          throw new Error(
            "android.input.multi_touch unsupported capability: runtime.multiTouch is unavailable; operator action is required",
          );
        }
        return runtime.multiTouch(normalizeMultiTouchPointers(action.pointers), {
          durationMs: normalizeMultiTouchDuration(action.duration_ms),
          timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
        });
      case "scroll": {
        const swipe = buildScrollSwipe(action, {
          frameWidth: readNumber(action, ["frame", "width"], "frameWidth", "frame_width"),
          frameHeight: readNumber(action, ["frame", "height"], "frameHeight", "frame_height"),
        });
        return runtime.swipe(
          swipe.x1,
          swipe.y1,
          swipe.x2,
          swipe.y2,
          {
            timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
            expectScrollChange: true,
          },
        );
      }
      case "open_url":
        if (typeof runtime.openUrl !== "function") {
          throw new Error("runtime.openUrl is unavailable");
        }
        return runtime.openUrl(requireString(action.url, "url"), {
          timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
          waitForSelector: action.wait_for_selector ?? null,
          waitForActivity: action.wait_for_activity ?? null,
          waitForPackage: action.wait_for_package ?? null,
        });
      case "set_orientation":
        if (typeof runtime.setOrientation !== "function") {
          throw new Error("runtime.setOrientation is unavailable");
        }
        return runtime.setOrientation(requireString(action.orientation, "orientation"), {
          timeoutSecs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
          waitForSelector: action.wait_for_selector ?? null,
        });
      case "wait": {
        const waitMs = optionalNumber(action.ms) ?? 1000;
        await new Promise((resolve) => setTimeout(resolve, waitMs));
        return {
          ok: true,
          waited_ms: waitMs,
          serial: runtime.getState().currentSerial,
        };
      }
      case "move":
        return {
          ok: true,
          note: "move is a no-op on Android touch surfaces",
          serial: runtime.getState().currentSerial,
        };
      case "zoom":
      case "reset_zoom":
        throw new Error(`${action.type} requires view metadata in android_step.view`);
      case "semantic_action":
        if (typeof runtime.semanticAction !== "function") {
          throw new Error("runtime.semanticAction is unavailable");
        }
        const semanticActionName = requireString(action.name ?? action.action_name, "name");
        const semanticTarget = action.target ?? null;
        let bodyQuery = action.body_query ?? null;
        if (bodyQuery == null && semanticActionName === "focus_body") {
          if (typeof semanticTarget === "string") {
            bodyQuery = semanticTarget;
          } else if (
            semanticTarget &&
            typeof semanticTarget === "object" &&
            !Array.isArray(semanticTarget)
          ) {
            const normalizedTarget = normalizeSelectorAliases(semanticTarget);
            const selectorQuery =
              normalizedTarget.text ??
              normalizedTarget.text_exact ??
              normalizedTarget.label ??
              normalizedTarget.label_exact ??
              normalizedTarget.content_desc;
            if (typeof selectorQuery === "string") {
              bodyQuery = selectorQuery;
            }
          }
        }
        return runtime.semanticAction(semanticActionName, {
          target: semanticTarget,
          bodyQuery,
          timeout_secs: typeof action.timeout_secs === "number" ? action.timeout_secs : 5,
        });
      default:
        throw new Error(`Unsupported android_step action: ${action.type}`);
    }
  }

  async function handleStep(args) {
    await ensureSerial();
    const actions = normalizeStepActions(args);
    const invalidAction = actions.find((action) => !STEP_ACTION_VALUES.includes(action.type));
    if (invalidAction) {
      throw new Error(`android_step action must be one of: ${STEP_ACTION_VALUES.join(", ")}`);
    }
    const postObserveScope = OBSERVE_SCOPE_VALUES.includes(args?.post_observe_scope)
      ? args.post_observe_scope
      : "screen_and_ui";
    let bridge = createComputerBridge(args);
    const usesBatchedActions = Array.isArray(args?.actions);
    const stepDefaults = {
      defaultPackageName:
        (usesBatchedActions ? (args?.package_name ?? args?.package) : null) ??
        defaultPackageName ??
        null,
      defaultActivity:
        (usesBatchedActions ? args?.activity : null) ??
        defaultActivity ??
        null,
    };

    const actionResults = [];
    for (const action of actions) {
      if (bridge && VIEW_GEOMETRY_INVALIDATING_ACTION_VALUES.has(action.type)) {
        bridge = null;
      } else if (!bridge && args?.view && bridgeReadyAction(action)) {
        throw new Error(
          `${action.type} cannot safely use stale android_step.view geometry after a posture change; split the rotation and coordinate action into separate android_step calls with a fresh android_observe`,
        );
      }
      actionResults.push(await performStepAction(action, bridge, stepDefaults));
    }
    const { context, observeWarning, view } = await observeAfterStep(
      actions,
      postObserveScope,
      bridge,
    );
    const uiDigest =
      postObserveScope === "screen_and_ui"
        && context.uiDumpPath
        ? await summarizeUiDump(context.uiDumpPath, readArtifactFile)
        : null;
    const visualWarning = combineObserveWarnings(
      observeWarning,
      nativeImageWarning(context, "android_step post-action observation"),
    );
    const outcome = outcomeFromStep({ actions, actionResults, observeWarning: visualWarning });
    const hasNativeImage = Boolean(context?.screenshotPath);

    return createDynamicToolCallResponse(
      await visualContextContentItems(context, {
        summaryLines: stepSummaryLines({
          actions,
          actionResults,
          context,
          uiDigest,
          observeWarning: visualWarning,
          view,
          outcome,
        }),
      }),
      {
        success: outcome.status !== "postcondition_failed" && hasNativeImage,
        metadata: {
          android: {
            outcome,
          },
        },
      },
    );
  }

  async function handleInstallBuildFromRun(rawArgs) {
    const args = normalizeInstallBuildFromRunArgs(rawArgs);
    if (typeof runtime.installBuildFromRun !== "function") {
      throw new Error("runtime.installBuildFromRun is unavailable");
    }
    const installResult = await runtime.installBuildFromRun(args);
    const postObserveScope = OBSERVE_SCOPE_VALUES.includes(rawArgs?.post_observe_scope)
      ? rawArgs.post_observe_scope
      : "screen_and_ui";
    const { context, observeWarning } = await captureObserveContext({
      scope: postObserveScope,
      actionLabel: "install-build",
      screenshotLabel: "codex-install-build-after",
      hierarchyFilename: "codex-install-build-after.xml",
      screenshotFilename: "codex-install-build-after.png",
    });
    const uiDigest =
      postObserveScope === "screen_and_ui"
        && context.uiDumpPath
        ? await summarizeUiDump(context.uiDumpPath, readArtifactFile)
        : null;
    const visualWarning = combineObserveWarnings(
      observeWarning,
      nativeImageWarning(context, "android_install_build_from_run post-install observation"),
    );
    const outcome = outcomeFromInstall({ result: installResult, observeWarning: visualWarning });
    const hasNativeImage = Boolean(context?.screenshotPath);

    return createDynamicToolCallResponse(
      await visualContextContentItems(context, {
        summaryLines: installSummaryLines({
          args,
          result: installResult,
          context,
          uiDigest,
          observeWarning: visualWarning,
          outcome,
        }),
      }),
      {
        success: outcome.status !== "postcondition_failed" && hasNativeImage,
        metadata: {
          android: {
            outcome,
          },
        },
      },
    );
  }

  return {
    getToolSpecs() {
      return [
        {
          name: TOOL_ANDROID_OBSERVE,
          description:
            "Capture the current Android screen as a model-visible screenshot, optionally with a compact UI digest.",
          inputSchema: {
            type: "object",
            properties: {
              prompt: { type: "string" },
              scope: {
                type: "string",
                enum: [...OBSERVE_SCOPE_VALUES],
              },
            },
            additionalProperties: false,
          },
          deferLoading: false,
          persistOnResume: false,
          capability: {
            family: "android",
            capabilityScope: "environment",
            mutationClass: "read_only",
            leaseMode: "shared_read",
          },
        },
        {
          name: TOOL_ANDROID_STEP,
          description:
            "Perform one or more bounded Android actions, then return a fresh post-action screenshot, summary, and current view metadata.",
          inputSchema: {
            type: "object",
            properties: {
              action: {
                type: "string",
                enum: [...STEP_ACTION_VALUES],
              },
              actions: {
                type: "array",
                items: { type: "object" },
              },
              post_observe_scope: {
                type: "string",
                enum: [...OBSERVE_SCOPE_VALUES],
              },
              view: {
                type: "object",
              },
              package_name: { type: "string" },
              activity: { type: "string" },
              selector: {},
              text: { type: "string" },
              x: { type: "number" },
              y: { type: "number" },
              x1: { type: "number" },
              y1: { type: "number" },
              x2: { type: "number" },
              y2: { type: "number" },
              pointers: {
                type: "array",
                minItems: 2,
                maxItems: 5,
                items: {
                  type: "object",
                  properties: {
                    x1: { type: "integer", minimum: 0 },
                    y1: { type: "integer", minimum: 0 },
                    x2: { type: "integer", minimum: 0 },
                    y2: { type: "integer", minimum: 0 },
                  },
                  required: ["x1", "y1", "x2", "y2"],
                  additionalProperties: false,
                },
              },
              keycode: { type: "string" },
              url: { type: "string" },
              orientation: { type: "string" },
              wait_ms: { type: "number" },
              timeout_ms: { type: "number" },
              timeout_secs: { type: "number" },
              duration_ms: { type: "number" },
              name: { type: "string" },
              action_name: { type: "string" },
              body_query: { type: "string" },
              wait_for_selector: {},
              wait_for_activity: { type: "string" },
              wait_for_package: { type: "string" },
              expect_focus_selector: {},
              expect_scroll_change: { type: "boolean" },
              wait_until_absent: { type: "boolean" },
              match_index: { type: "number" },
              target: {},
            },
            additionalProperties: false,
          },
          deferLoading: false,
          persistOnResume: false,
          capability: {
            family: "android",
            capabilityScope: "environment",
            mutationClass: "mutating",
            leaseMode: "exclusive_write",
          },
        },
        {
          name: TOOL_ANDROID_INSTALL_BUILD_FROM_RUN,
          description:
            "Install a GitHub Actions Android build artifact into the active Android session, optionally launch it, then return a post-install observation when available.",
          inputSchema: {
            type: "object",
            properties: {
              workflow_run_id: { type: "integer" },
              artifact_name: { type: "string" },
              repository: { type: "string" },
              launch_after_install: { type: "boolean" },
              contract_version: {
                type: "string",
                const: "android-provider-execution/v1",
              },
              target: { type: "object" },
              install: {
                type: "object",
                properties: {
                  launch_after_install: { type: "boolean" },
                },
                required: ["launch_after_install"],
                additionalProperties: false,
              },
              serial: { type: "string" },
              timeout_secs: { type: "integer" },
              post_observe_scope: {
                type: "string",
                enum: [...OBSERVE_SCOPE_VALUES],
              },
            },
            required: ["workflow_run_id", "artifact_name"],
            allOf: [{
              if: { required: ["contract_version"] },
              then: {
                properties: {
                  launch_after_install: { type: "null" },
                  target: {
                    type: "object",
                    required: ["expected_build"],
                  },
                },
                required: ["target", "install"],
              },
              else: {
                properties: {
                  install: { type: "null" },
                },
              },
            }],
            additionalProperties: false,
          },
          deferLoading: false,
          persistOnResume: false,
          capability: {
            family: "android",
            capabilityScope: "environment",
            mutationClass: "mutating",
            leaseMode: "exclusive_write",
          },
        },
      ];
    },

    async executeToolCall(call) {
      const tool = call?.tool;
      const args = call?.arguments ?? {};
      try {
        switch (tool) {
          case TOOL_ANDROID_OBSERVE:
            return await handleObserve(args);
          case TOOL_ANDROID_STEP:
            return await handleStep(args);
          case TOOL_ANDROID_INSTALL_BUILD_FROM_RUN:
            return await handleInstallBuildFromRun(args);
          default:
            throw new Error(`Unsupported Codex Android dynamic tool: ${tool}`);
        }
      } catch (error) {
        return createFailureResponse({
          tool: isKnownTool(tool) ? tool : TOOL_ANDROID_STEP,
          args,
          error,
        });
      }
    },
  };
}

export { contextFromObservation } from "./codex_thread_items.js";
export { createDynamicToolCallResponse };
