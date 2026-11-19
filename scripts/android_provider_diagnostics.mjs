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
