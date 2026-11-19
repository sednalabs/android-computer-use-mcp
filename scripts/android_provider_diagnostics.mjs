// This module emits only bounded build identifiers and categorical ANR evidence.
import { setTimeout as delay } from "node:timers/promises";

export function summarizeBuildIdentity({ apiLevel = "", release = "", buildId = "", fingerprint = "", emulator = "", adb = "" }) {
  const safe = (value, pattern) => pattern.test(value.trim()) ? value.trim() : undefined;
  const api = safe(apiLevel, /^[1-9]\d{0,2}$/);
  const emulatorVersion = emulator.slice(0, 2048).match(/^(?:INFO\s*\|\s*)?Android emulator version ([\d.]{1,24}) \(build_id (\d{1,16})\)(?: \(CL:[A-Za-z0-9/:._-]{1,32}\))?/m);
  const adbProtocol = adb.slice(0, 2048).match(/^Android Debug Bridge version ([\d.]{1,24})$/m);
  const adbBuild = adb.slice(0, 2048).match(/^Version ([A-Za-z0-9._-]{1,48})$/m);
  return {
    android_api_level: api ? Number(api) : undefined,
    android_release: safe(release, /^[A-Za-z0-9._-]{1,32}$/),
    android_build_id: safe(buildId, /^[A-Za-z0-9._-]{1,64}$/),
    android_build_fingerprint: safe(fingerprint, /^[A-Za-z0-9._:/+-]{1,256}$/),
    emulator_version: emulatorVersion?.[1], emulator_build_id: emulatorVersion?.[2],
    emulator_version_line: emulatorVersion?.[0],
    adb_protocol_version: adbProtocol?.[1], adb_build_version: adbBuild?.[1],
  };
}

export function decodeXmlAttribute(value) {
  if (typeof value !== "string") return undefined;
  const entities = { apos: "'", quot: '"', amp: "&", lt: "<", gt: ">" };
  return value.replace(/&(apos|quot|amp|lt|gt|#\d{1,7}|#x[0-9A-Fa-f]{1,6});/g, (whole, entity) => {
    if (entities[entity]) return entities[entity];
    const codepoint = entity.startsWith("#x") ? Number.parseInt(entity.slice(2), 16) : Number(entity.slice(1));
    return codepoint <= 0x10ffff && !(codepoint >= 0xd800 && codepoint <= 0xdfff)
      ? String.fromCodePoint(codepoint) : whole;
  });
}

export function classifyAnrOwner(nodes) {
  if (!nodes.some((node) => node.package === "android" && node["resource-id"] === "android:id/aerr_wait")) return undefined;
  const titles = nodes.filter((node) => node.package === "android" && node["resource-id"] === "android:id/alertTitle")
    .map((node) => decodeXmlAttribute(node.text)?.trim().replaceAll("’", "'"));
  if (titles.length !== 1) return "unknown";
  const owners = new Map([
    ["System UI isn't responding", "system_ui"],
    ["System UI is not responding", "system_ui"],
    ["Android MCP Acceptance isn't responding", "acceptance_fixture"],
    ["Android MCP Acceptance is not responding", "acceptance_fixture"],
    ["Pixel Launcher isn't responding", "other"],
    ["Launcher3 isn't responding", "other"],
    ["Process system isn't responding", "other"],
  ]);
  return owners.get(titles[0]) || "unknown";
}

const knownAnrProcesses = new Map([
  ["com.android.systemui", "system_ui"],
  ["com.sednalabs.mcp.acceptance", "acceptance_fixture"],
  ["com.android.launcher3", "launcher"],
  ["com.google.android.apps.nexuslauncher", "launcher"],
  ["system_server", "system_server"],
]);
const anrOwner = (category) => ["system_ui", "acceptance_fixture"].includes(category)
  ? category : category === "unknown" ? "unknown" : "other";

function boundedDiagnosticLines(output, maxBytes = 2 * 1024 * 1024) {
  if (typeof output !== "string") return { available: false, reason: "unavailable" };
  if (Buffer.byteLength(output, "utf8") > maxBytes) return { available: false, reason: "bounds_exceeded" };
  const lines = output.split(/\r?\n/);
  if (lines.length > 32768 || lines.some((line) => line.length > 8192)) return { available: false, reason: "bounds_exceeded" };
  return { available: true, lines };
}

// AppNotRespondingDialog sets a nonlocalized window title containing the process
// name. Only source-defined main Window # blocks count; other dump references do
// not establish which ANR dialog is currently displayed. No raw names survive.
// Android 15: AppNotRespondingDialog.java, RootWindowContainer.dumpWindowsNoHeader,
// and WindowState.dump/toString in frameworks/base/services/core/java/com/android/server.
export function summarizeCurrentAnrWindow(output, systemControlsObserved = false) {
  const parsed = boundedDiagnosticLines(output);
  if (!parsed.available) return parsed;
  const summary = { available: true, system_controls_observed: systemControlsObserved === true, anr_window_count: 0, active_anr_window_count: 0, owner: "unknown" };
  const blocks = [];
  let block;
  for (const line of parsed.lines) {
    if (/^  Window #/.test(line)) {
      block = undefined;
      if (!line.includes(" Application Not Responding: ")) continue;
      const match = line.match(/^  Window #\d{1,5} Window\{[a-fA-F0-9]{1,16} u\d{1,6} Application Not Responding: ([^\r\n}]{1,200})\}:$/);
      block = { title: match?.[1], lines: [] };
      blocks.push(block);
      if (blocks.length > 128) return { available: false, reason: "bounds_exceeded" };
      continue;
    }
    if (block && line.length - line.trimStart().length >= 4) block.lines.push(line);
    else if (line.trim()) block = undefined;
  }
  summary.anr_window_count = blocks.length;
  const active = [];
  let ambiguous = false;
  for (const candidate of blocks) {
    const exits = candidate.lines.filter((line) => /^    mAnimatingExit=/.test(line));
    if (candidate.title?.endsWith(" EXITING")) continue;
    if (exits.length > 1 || (exits.length === 1
      && !/^    mAnimatingExit=(true|false) mRemoveOnExit=(true|false) mDestroying=(true|false) mRemoved=(true|false)$/.test(exits[0]))) {
      ambiguous = true; continue;
    }
    if (exits.length && /\b(?:mAnimatingExit|mRemoveOnExit|mDestroying|mRemoved)=true\b/.test(exits[0])) continue;
    const owners = candidate.lines.filter((line) => /^    mOwnerUid=/.test(line));
    const surfaces = candidate.lines.filter((line) => /^    mHasSurface=/.test(line));
    const visible = candidate.lines.filter((line) => /^    isVisible=/.test(line));
    if (surfaces.length === 1 && /^    mHasSurface=(?:false|true) isReadyForDisplay\(\)=false\b/.test(surfaces[0])) continue;
    if (visible.length === 1 && visible[0] === "    isVisible=false") continue;
    if (!candidate.title || owners.length !== 1 || surfaces.length !== 1 || visible.length !== 1
      || !/^    mOwnerUid=1000 showForAllUsers=(?:true|false) package=android(?:\s|$)/.test(owners[0])
      || !/^    mHasSurface=true isReadyForDisplay\(\)=true\b/.test(surfaces[0])
      || visible[0] !== "    isVisible=true") { ambiguous = true; continue; }
    active.push(knownAnrProcesses.get(candidate.title) || "unknown");
  }
  summary.active_anr_window_count = active.length;
  if (ambiguous) summary.ambiguous = true;
  if (systemControlsObserved === true && !ambiguous && active.length === 1) {
    summary.process_category = active[0];
    summary.owner = anrOwner(active[0]);
  }
  return summary;
}

const knownAnrFrames = new Set([
  "android.view.ViewDebug.dispatchCommand", "android.view.ViewDebug.dumpEncoded",
  "android.view.ViewRootImpl$W.executeCommand", "android.view.View.encode",
  "android.view.ViewGroup.encodeProperties", "android.os.BinderProxy.transactNative",
  "android.os.BinderProxy.transact", "android.os.Binder.execTransactInternal",
  "android.os.Binder.execTransact", "libcore.io.Linux.write", "libcore.io.IoBridge.write",
  "java.io.FileOutputStream.write", "android.os.MessageQueue.nativePollOnce",
  "android.os.MessageQueue.next", "android.os.Looper.loop", "android.os.Looper.loopOnce",
  "android.os.Handler.handleCallback", "android.os.Handler.dispatchMessage",
  "android.app.ActivityThread.main", "android.app.ActivityThread.handleBindApplication",
  "android.app.LoadedApk.makeApplicationInner", "android.app.Instrumentation.callApplicationOnCreate",
  "com.android.systemui.SystemUIApplication.onCreate", "java.lang.Object.wait",
  "java.lang.Thread.sleep", "java.util.concurrent.CountDownLatch.await", "java.util.concurrent.FutureTask.get",
]);
const knownAnrThreadStates = new Set([
  "Native", "Runnable", "Sleeping", "Blocked", "Waiting", "TimedWaiting", "Suspended",
  "WaitingForGcToComplete", "WaitingPerformingGc", "WaitingForGcThreadFlip",
  "WaitingForJniOnLoad", "WaitingForLockInflation", "WaitingForCheckPointsToRun",
]);
const observerFrames = new Set([
  "android.view.ViewDebug.dispatchCommand", "android.view.ViewDebug.dumpEncoded",
  "android.view.ViewRootImpl$W.executeCommand", "android.view.View.encode", "android.view.ViewGroup.encodeProperties",
]);

function anrReasonCategory(reason) {
  if (typeof reason !== "string") return "unknown";
  // Only fixed Android reason templates are classified; the app, window,
  // intent and input-event descriptions are private and never returned.
  if (reason.startsWith("Input dispatching timed out")) return "input_dispatch";
  if (/^.{1,512} is not responding\. Waited \d{1,10}ms for .{1,4096}$/.test(reason)) return "input_dispatch";
  if (/^.{1,512} does not have a focused window$/.test(reason)) return "input_dispatch_no_focused_window";
  if (reason.startsWith("Broadcast of ")) return "broadcast_receiver";
  if (reason.startsWith("executing service ")) return "service_execution";
  if (reason.startsWith("Context.startForegroundService() did not then call Service.startForeground(): ")) return "service_start";
  return "unknown";
}

// `lastanr-traces` selects the most recently modified /data/anr file, including
// temporary predumps, without association to the currently displayed ANR.
// ART delimits processes with pid/end headers and an
// exact Cmd line field. Keep only known process categories and literal known
// frame symbols; neither process IDs, thread names nor arbitrary stack data is
// retained. Every included process is historical captured context, not blame.
export function summarizeAnrTraces(output) {
  const parsed = boundedDiagnosticLines(output);
  if (!parsed.available) return parsed;
  const summary = { available: true, relation: "latest_anr_directory_file", reason_category: "unknown", processes: [] };
  const subjects = [];
  const blocks = [];
  let block;
  let thread;
  let processStarted = false;
  for (const line of parsed.lines) {
    if (!processStarted && line.startsWith("Subject: ")) subjects.push(line.slice(9));
    if (line.startsWith("----- pid ")) {
      processStarted = true;
      thread = undefined;
      block = undefined;
      const header = line.match(/^----- pid (\d{1,10}) at [^\r\n]{1,80} -----$/);
      if (header) {
        block = { pid: header[1], commands: [], threads: [], complete: false };
        blocks.push(block);
        if (blocks.length > 64) return { available: false, reason: "bounds_exceeded" };
      }
      continue;
    }
    if (line.startsWith("----- end ")) {
      if (block) block.complete = line === `----- end ${block.pid} -----`;
      block = undefined;
      thread = undefined;
      continue;
    }
    if (!block) continue;
    if (line.startsWith("Cmd line: ")) {
      block.commands.push(knownAnrProcesses.get(line.slice(10)) || "unknown");
      thread = undefined;
      continue;
    }
    if (line.startsWith('"')) {
      thread = undefined;
      const header = line.match(/^"([^"\r\n]{1,256})"(?: daemon)? prio=\d{1,2} tid=\d{1,6} ([A-Za-z]+)(?: .*)?$/);
      if (header && block.commands.length === 1 && block.commands[0] !== "unknown") {
        thread = {
          category: header[1] === "main" ? "main" : /^Binder:\d+_[0-9A-Fa-f]+$/.test(header[1]) ? "binder" : "other",
          state: knownAnrThreadStates.has(header[2]) ? header[2] : "unknown",
          known_frames: [],
        };
        block.threads.push(thread);
        if (block.threads.length > 2048) return { available: false, reason: "bounds_exceeded" };
      }
      continue;
    }
    const frame = thread && line.match(/^\s+at ([A-Za-z0-9_.$]+)\([^\r\n]*\)$/);
    if (frame && knownAnrFrames.has(frame[1]) && !thread.known_frames.includes(frame[1])) {
      thread.known_frames.push(frame[1]);
    }
  }
  if (subjects.length === 1) summary.reason_category = anrReasonCategory(subjects[0]);
  if (subjects.length > 1) summary.ambiguous_subject = true;
  let omitted = false;
  for (const candidate of blocks) {
    if (candidate.commands.length !== 1 || candidate.commands[0] === "unknown") { omitted = true; continue; }
    if (!candidate.complete) { summary.incomplete_process_sections = true; omitted = true; continue; }
    const priority = (item) => item.category === "main" ? 0
      : item.known_frames.some((frame) => observerFrames.has(frame)) ? 1 : 2;
    const relevant = candidate.threads.filter((item) => item.category === "main" || item.known_frames.length)
      .sort((left, right) => priority(left) - priority(right));
    if (summary.processes.length >= 8) { omitted = true; continue; }
    summary.processes.push({
      process_category: candidate.commands[0], complete: candidate.complete,
      frames_allowlisted_only: true, threads: relevant.slice(0, 12),
      threads_omitted: relevant.length > 12,
    });
  }
  summary.processes_omitted = omitted;
  return summary;
}

export function summarizeVerification(verification) {
  if (!verification) return undefined;
  return {
    requested: verification.requested, satisfied: verification.satisfied,
    timed_out: verification.timed_out, elapsed_ms: verification.elapsed_ms,
    stabilized: verification.stabilized,
    stable_polls_observed: verification.stable_polls_observed,
    stable_polls_required: verification.stable_polls_required,
    target_selector_still_present: verification.target_selector_still_present,
    target_text_matches_requested: verification.target_text_matches_requested,
    tapped_selector_still_present: verification.tapped_selector_still_present,
    post_selector_matched: verification.post_selector_matched,
    handled_system_dialogs: verification.handled_system_dialogs?.slice(0, 4).map((dialog) => ({
      kind: dialog.kind, action: dialog.action,
    })) || [],
  };
}

export function summarizeCredentialReadiness(output, currentUser) {
  const summary = { current_user: currentUser, state: "failed" };
  if (!Number.isSafeInteger(currentUser) || currentUser < 0 || currentUser > 999999) return summary;
  if (typeof output !== "string" || output.length > 1024 * 1024) return summary;
  const lines = output.split(/\r?\n/);
  if (lines.length > 8192 || lines.some((line) => line.length > 2048)) return summary;
  let userStateHeaders = 0;
  let userStateIndent;
  let block;
  const blocks = [];
  const startedFlags = [];
  for (const line of lines) {
    if (!line.trim()) continue;
    const token = line.trim();
    const indent = line.length - line.trimStart().length;
    if (token.startsWith("ThirdPartyAppsStarted:")) {
      if (indent !== 0) return summary;
      startedFlags.push(token === "ThirdPartyAppsStarted: true" ? true
        : token === "ThirdPartyAppsStarted: false" ? false : undefined);
    }
    if (token === "User State:") {
      if (indent !== 0) return summary;
      userStateHeaders++;
      userStateIndent = indent;
      block = undefined;
      continue;
    }
    if (token.startsWith("CredentialType:")) {
      if (!block || userStateIndent === undefined || indent !== block.indent + 2) return summary;
      block.types.push(token.match(/^CredentialType: (NONE|PIN|PASSWORD|PATTERN)$/)?.[1]);
      continue;
    }
    if (userStateIndent === undefined) continue;
    if (indent <= userStateIndent) { userStateIndent = undefined; block = undefined; continue; }
    const user = token.match(/^User (\d{1,6})$/);
    if (user) {
      if (indent !== userStateIndent + 2) return summary;
      block = { user: Number(user[1]), indent, types: [] };
      blocks.push(block);
      continue;
    }
    if (block && indent <= block.indent) block = undefined;
  }
  const matches = blocks.filter((candidate) => candidate.user === currentUser);
  if (userStateHeaders !== 1 || matches.length !== 1 || matches[0].types.length !== 1 || startedFlags.length !== 1) return summary;
  summary.credential_type = matches[0].types[0];
  summary.third_party_apps_started = startedFlags[0];
  // Android 15 may report NONE before user initialization. Require its explicit
  // ThirdPartyAppsStarted flag as well; never infer security from a missing field.
  // LockSettingsService.dumpInternal reads getCredentialTypeInternal directly.
  // https://android.googlesource.com/platform/frameworks/base/+/refs/tags/android-15.0.0_r1/services/core/java/com/android/server/locksettings/LockSettingsService.java
  if (summary.credential_type === "NONE") {
    if (summary.third_party_apps_started === true) summary.state = "ready";
    else if (summary.third_party_apps_started === false) summary.state = "pending";
  }
  return summary;
}

export function awakeUnlockedReadiness(state) {
  const field = (source, name) => {
    const matches = source?.available === true ? source.fields?.filter((item) => item.name === name) : [];
    return matches?.length === 1 ? matches[0].value : undefined;
  };
  const wakefulness = field(state?.power, "mWakefulness");
  const showing = field(state?.window_policy, "showing");
  const restricted = field(state?.window_policy, "inputRestricted");
  if (!["Awake", "Asleep", "Dreaming", "Dozing"].includes(wakefulness)
    || !["true", "false"].includes(showing) || !["true", "false"].includes(restricted)) return "failed";
  return wakefulness === "Awake" && showing === "false" && restricted === "false" ? "ready" : "pending";
}

export async function pollReadOnlyReadiness(read, classify, { timeoutMs = 30000, intervalMs = 250, now = () => performance.now(), wait = delay } = {}) {
  const started = now();
  let last;
  let polls = 0;
  while (now() - started < timeoutMs) {
    last = await read(timeoutMs - (now() - started));
    polls++;
    if (now() - started >= timeoutMs) break;
    const state = classify(last);
    if (state !== "pending") return { ready: state === "ready", last, polls, elapsed_ms: now() - started, timed_out: false };
    const remaining = timeoutMs - (now() - started);
    if (remaining <= 0) break;
    await wait(Math.min(intervalMs, remaining));
  }
  return { ready: false, last, polls, elapsed_ms: now() - started, timed_out: true };
}
