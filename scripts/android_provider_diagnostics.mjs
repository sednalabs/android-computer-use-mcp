// This module emits only bounded build identifiers and categorical ANR evidence.
export function summarizeBuildIdentity({ apiLevel = "", release = "", buildId = "", fingerprint = "", emulator = "", adb = "" }) {
  const safe = (value, pattern) => pattern.test(value.trim()) ? value.trim() : undefined;
  const api = safe(apiLevel, /^[1-9]\d{0,2}$/);
  const emulatorVersion = emulator.slice(0, 2048).match(/^Android emulator version ([\d.]{1,24}) \(build_id (\d{1,16})\)(?: \(CL:[A-Za-z0-9/:._-]{1,32}\))?/m);
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
