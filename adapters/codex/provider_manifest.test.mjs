import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import {
  ANDROID_PROVIDER_MANIFEST_DEFAULT_CAPABILITIES,
  ANDROID_PROVIDER_MANIFEST_OUTCOME_TAXONOMY,
  ANDROID_PROVIDER_MANIFEST_SCHEMA_VERSION,
  ANDROID_PROVIDER_MANIFEST_REQUIRED_TOOLS,
  createCodexAndroidProviderManifest,
  validateCodexAndroidProviderManifest,
} from "./provider_manifest.js";

test("provider manifest describes the generic Android computer-use capability", () => {
  const manifest = createCodexAndroidProviderManifest({
    mcpUrl: "https://android.example/mcp",
    defaultSerial: "emulator-5554",
    defaultPackageName: "com.example.app",
    defaultActivity: ".MainActivity",
    sessionRoot: "dist/interactive-session",
    artifactRoot: "dist/interactive-session/codex-bridge-runs",
    buildManifestPath: "dist/interactive-build-manifest.json",
    device: { serial: "emulator-5554", orientation: "portrait" },
    now: () => new Date("2026-04-26T00:00:00.000Z"),
  });

  assert.equal(manifest.schemaVersion, ANDROID_PROVIDER_MANIFEST_SCHEMA_VERSION);
  assert.deepEqual(ANDROID_PROVIDER_MANIFEST_REQUIRED_TOOLS, [
    "android_observe",
    "android_step",
    "android_install_build_from_run",
  ]);
  assert.deepEqual(manifest.environment.capabilities, ANDROID_PROVIDER_MANIFEST_DEFAULT_CAPABILITIES);
  assert.deepEqual(manifest.provider, {
    family: "android",
    adapter: "android",
    transport: "android-computer-use-mcp",
    toolNames: [
      "android_observe",
      "android_step",
      "android_install_build_from_run",
    ],
  });
  assert.equal(manifest.environment.scope, "environment");
  assert.equal(manifest.environment.mcpUrl, "https://android.example/mcp");
  assert.equal(manifest.environment.defaultPackageName, "com.example.app");
  assert.deepEqual(manifest.environment.device, {
    serial: "emulator-5554",
    orientation: "portrait",
  });
  assert.deepEqual(manifest.environment.capabilities.rawInput, [
    "tap",
    "double_tap",
    "long_press",
    "text",
    "swipe",
    "multi_touch",
    "keyevent",
    "keycombination",
  ]);
  assert.equal(manifest.policy.persistOnResume, false);
  assert.equal(manifest.policy.resumeBehavior, "revalidate_required");
  assert.deepEqual(manifest.policy.outcomeTaxonomy, ANDROID_PROVIDER_MANIFEST_OUTCOME_TAXONOMY);
  assert.deepEqual(manifest.policy.leases.android_observe, {
    mutationClass: "read_only",
    leaseMode: "shared_read",
  });
  assert.deepEqual(manifest.policy.leases.android_step, {
    mutationClass: "mutating",
    leaseMode: "exclusive_write",
  });
  assert.deepEqual(manifest.policy.leases.android_install_build_from_run, {
    mutationClass: "mutating",
    leaseMode: "exclusive_write",
  });
  assert.equal(manifest.status.generatedAt, "2026-04-26T00:00:00.000Z");
});

test("provider manifest validator returns a stable proof summary", () => {
  const manifest = createCodexAndroidProviderManifest({
    mcpUrl: "https://android.example/mcp",
    defaultPackageName: "com.example.app",
    defaultActivity: ".MainActivity",
    artifactRoot: "dist/interactive-session/codex-bridge-runs",
    buildManifestPath: "dist/interactive-build-manifest.json",
    now: () => new Date("2026-04-26T00:00:00.000Z"),
  });

  assert.deepEqual(validateCodexAndroidProviderManifest(manifest), {
    ok: true,
    schemaVersion: 4,
    provider: {
      family: "android",
      adapter: "android",
      transport: "android-computer-use-mcp",
      toolNames: [
        "android_observe",
        "android_step",
        "android_install_build_from_run",
      ],
    },
    environment: {
      scope: "environment",
      defaultPackageName: "com.example.app",
      defaultActivity: ".MainActivity",
      artifactRoot: "dist/interactive-session/codex-bridge-runs",
      buildManifestPath: "dist/interactive-build-manifest.json",
      capabilityGroups: ["appControl", "posture", "rawInput"],
    },
    policy: {
      timeoutMs: 120000,
      observeLease: {
        mutationClass: "read_only",
        leaseMode: "shared_read",
      },
      stepLease: {
        mutationClass: "mutating",
        leaseMode: "exclusive_write",
      },
      installLease: {
        mutationClass: "mutating",
        leaseMode: "exclusive_write",
      },
      resumeBehavior: "revalidate_required",
      outcomeTaxonomy: {
        statuses: [
          "succeeded",
          "postcondition_failed",
          "observe_degraded",
          "invalid_request",
          "stale_view",
          "unsupported_capability",
          "provider_unavailable",
          "failed",
        ],
        retryability: [
          "none",
          "observe_then_retry",
          "retry_same_request",
          "operator_required",
        ],
      },
    },
    status: {
      state: "ready",
      generatedAt: "2026-04-26T00:00:00.000Z",
    },
  });
});

test("provider manifest validator rejects missing native tool names", () => {
  const manifest = createCodexAndroidProviderManifest({
    mcpUrl: "https://android.example/mcp",
  });
  manifest.provider.toolNames = ["android_observe"];

  assert.throws(
    () => validateCodexAndroidProviderManifest(manifest),
    /provider\.toolNames must match the native tool contract exactly/,
  );
});

test("provider manifest validator rejects contract drift", () => {
  const manifest = createCodexAndroidProviderManifest({
    mcpUrl: "https://android.example/mcp",
  });
  manifest.provider.transport = "wrong-transport";

  assert.throws(
    () => validateCodexAndroidProviderManifest(manifest),
    /provider\.transport must be android-computer-use-mcp/,
  );
});

test("provider manifest validator rejects extra tool names", () => {
  const manifest = createCodexAndroidProviderManifest({
    mcpUrl: "https://android.example/mcp",
  });
  manifest.provider.toolNames.push("android_debug");

  assert.throws(
    () => validateCodexAndroidProviderManifest(manifest),
    /provider\.toolNames must match the native tool contract exactly/,
  );
});

test("provider manifest validator rejects invalid timeout types", () => {
  const manifest = createCodexAndroidProviderManifest({
    mcpUrl: "https://android.example/mcp",
  });
  manifest.policy.timeoutMs = "oops";

  assert.throws(
    () => validateCodexAndroidProviderManifest(manifest),
    /policy\.timeoutMs must be a positive integer/,
  );
});

test("provider manifest validator rejects capability drift", () => {
  const manifest = createCodexAndroidProviderManifest({
    mcpUrl: "https://android.example/mcp",
  });
  manifest.environment.capabilities.rawInput = ["tap"];

  assert.throws(
    () => validateCodexAndroidProviderManifest(manifest),
    /environment\.capabilities\.rawInput must match the native Android contract/,
  );
});

test("provider manifest validator rejects outcome taxonomy drift", () => {
  const manifest = createCodexAndroidProviderManifest({
    mcpUrl: "https://android.example/mcp",
  });
  manifest.policy.outcomeTaxonomy.statuses = ["succeeded"];

  assert.throws(
    () => validateCodexAndroidProviderManifest(manifest),
    /policy\.outcomeTaxonomy\.statuses must match the native Android contract/,
  );
});

test("provider manifest validator rejects non-string optional summary fields", () => {
  const manifest = createCodexAndroidProviderManifest({
    mcpUrl: "https://android.example/mcp",
  });
  manifest.environment.defaultPackageName = 42;

  assert.throws(
    () => validateCodexAndroidProviderManifest(manifest),
    /environment\.defaultPackageName must be a string or null/,
  );
});

test("provider manifest validator rejects non-string generated timestamps", () => {
  const manifest = createCodexAndroidProviderManifest({
    mcpUrl: "https://android.example/mcp",
  });
  manifest.status.generatedAt = {};

  assert.throws(
    () => validateCodexAndroidProviderManifest(manifest),
    /status\.generatedAt must be a non-empty string/,
  );
});

test("provider manifest validator rejects reordered tool names", () => {
  const manifest = createCodexAndroidProviderManifest({
    mcpUrl: "https://android.example/mcp",
  });
  manifest.provider.toolNames = ["android_step", "android_observe"];

  assert.throws(
    () => validateCodexAndroidProviderManifest(manifest),
    /provider\.toolNames must match the native tool contract exactly/,
  );
});

test("provider manifest contract constants are immutable and generation clones them", () => {
  assert.throws(
    () => {
      ANDROID_PROVIDER_MANIFEST_REQUIRED_TOOLS.push("android_debug");
    },
    /Cannot add property|object is not extensible|read only/,
  );
  assert.throws(
    () => {
      ANDROID_PROVIDER_MANIFEST_DEFAULT_CAPABILITIES.rawInput.push("debug");
    },
    /Cannot add property|object is not extensible|read only/,
  );
  assert.throws(
    () => {
      ANDROID_PROVIDER_MANIFEST_OUTCOME_TAXONOMY.statuses.push("debug");
    },
    /Cannot add property|object is not extensible|read only/,
  );

  const manifest = createCodexAndroidProviderManifest({
    mcpUrl: "https://android.example/mcp",
  });
  manifest.environment.capabilities.rawInput.push("debug");

  assert.deepEqual(ANDROID_PROVIDER_MANIFEST_DEFAULT_CAPABILITIES.rawInput, [
    "tap",
    "double_tap",
    "long_press",
    "text",
    "swipe",
    "multi_touch",
    "keyevent",
    "keycombination",
  ]);
});

test("validate-manifest CLI validates stdin manifests directly", () => {
  const cliPath = fileURLToPath(new URL("./bin/codex-android-tools.mjs", import.meta.url));
  const manifest = createCodexAndroidProviderManifest({
    mcpUrl: "https://android.example/mcp",
    now: () => new Date("2026-04-26T00:00:00.000Z"),
  });

  const output = execFileSync(
    process.execPath,
    [cliPath, "validate-manifest"],
    {
      input: JSON.stringify(manifest),
      encoding: "utf8",
    },
  );
  const summary = JSON.parse(output);

  assert.equal(summary.ok, true);
  assert.deepEqual(summary.provider.toolNames, [
    "android_observe",
    "android_step",
    "android_install_build_from_run",
  ]);
});

test("provider manifest requires an MCP URL", () => {
  assert.throws(
    () => createCodexAndroidProviderManifest(),
    /requires an MCP URL/,
  );
});
