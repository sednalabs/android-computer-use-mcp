export const ANDROID_PROVIDER_MANIFEST_SCHEMA_VERSION = 4;
export const ANDROID_PROVIDER_MANIFEST_REQUIRED_TOOLS = Object.freeze([
  "android_observe",
  "android_step",
  "android_install_build_from_run",
]);
export const ANDROID_PROVIDER_MANIFEST_DEFAULT_CAPABILITIES = Object.freeze({
  appControl: Object.freeze(["list_apps", "launch_app", "open_url", "terminate_app", "uninstall_app"]),
  posture: Object.freeze(["get_orientation", "set_orientation"]),
  rawInput: Object.freeze(["tap", "double_tap", "long_press", "text", "swipe", "multi_touch", "keyevent", "keycombination"]),
});
export const ANDROID_PROVIDER_MANIFEST_OUTCOME_TAXONOMY = Object.freeze({
  statuses: Object.freeze([
    "succeeded",
    "postcondition_failed",
    "observe_degraded",
    "invalid_request",
    "stale_view",
    "unsupported_capability",
    "provider_unavailable",
    "failed",
  ]),
  retryability: Object.freeze([
    "none",
    "observe_then_retry",
    "retry_same_request",
    "operator_required",
  ]),
});

function cloneCapabilities(capabilities) {
  return Object.fromEntries(
    Object.entries(capabilities).map(([groupName, groupCapabilities]) => [
      groupName,
      [...groupCapabilities],
    ]),
  );
}

export function createCodexAndroidProviderManifest({
  mcpUrl,
  defaultSerial = null,
  defaultPackageName = null,
  defaultActivity = null,
  sessionRoot = null,
  artifactRoot = null,
  buildManifestPath = null,
  device = null,
  capabilities = null,
  timeoutMs = 120000,
  now = () => new Date(),
} = {}) {
  if (!mcpUrl) {
    throw new Error("Codex Android provider manifest requires an MCP URL");
  }

  return {
    schemaVersion: ANDROID_PROVIDER_MANIFEST_SCHEMA_VERSION,
    provider: {
      family: "android",
      adapter: "android",
      transport: "android-computer-use-mcp",
      toolNames: [...ANDROID_PROVIDER_MANIFEST_REQUIRED_TOOLS],
    },
    environment: {
      scope: "environment",
      serial: defaultSerial,
      mcpUrl,
      sessionRoot,
      artifactRoot,
      buildManifestPath,
      defaultPackageName,
      defaultActivity,
      device,
      capabilities: cloneCapabilities(
        capabilities ?? ANDROID_PROVIDER_MANIFEST_DEFAULT_CAPABILITIES,
      ),
    },
    policy: {
      timeoutMs,
      persistOnResume: false,
      resumeBehavior: "revalidate_required",
      outcomeTaxonomy: {
        statuses: [...ANDROID_PROVIDER_MANIFEST_OUTCOME_TAXONOMY.statuses],
        retryability: [...ANDROID_PROVIDER_MANIFEST_OUTCOME_TAXONOMY.retryability],
      },
      leases: {
        android_observe: {
          mutationClass: "read_only",
          leaseMode: "shared_read",
        },
        android_step: {
          mutationClass: "mutating",
          leaseMode: "exclusive_write",
        },
        android_install_build_from_run: {
          mutationClass: "mutating",
          leaseMode: "exclusive_write",
        },
      },
    },
    status: {
      state: "ready",
      generatedAt: now().toISOString(),
    },
  };
}

function requireObject(value, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`Codex Android provider manifest ${label} must be an object`);
  }
  return value;
}

function requireString(value, label) {
  if (typeof value !== "string" || !value.trim()) {
    throw new Error(`Codex Android provider manifest ${label} must be a non-empty string`);
  }
  return value;
}

function requireArray(value, label) {
  if (!Array.isArray(value)) {
    throw new Error(`Codex Android provider manifest ${label} must be an array`);
  }
  return value;
}

function arraysEqual(left, right) {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

function requireNullableString(value, label) {
  if (value != null && typeof value !== "string") {
    throw new Error(`Codex Android provider manifest ${label} must be a string or null`);
  }
  return value ?? null;
}

function requireToolNames(manifest) {
  const toolNames = requireArray(manifest.provider.toolNames, "provider.toolNames");
  for (let index = 0; index < ANDROID_PROVIDER_MANIFEST_REQUIRED_TOOLS.length; index += 1) {
    if (toolNames[index] !== ANDROID_PROVIDER_MANIFEST_REQUIRED_TOOLS[index]) {
      throw new Error(`Codex Android provider manifest provider.toolNames must match the native tool contract exactly`);
    }
  }
  if (toolNames.length !== ANDROID_PROVIDER_MANIFEST_REQUIRED_TOOLS.length) {
    throw new Error("Codex Android provider manifest provider.toolNames must match the native tool contract exactly");
  }
  return toolNames;
}

function requireExactString(value, label, expected) {
  if (value !== expected) {
    throw new Error(`Codex Android provider manifest ${label} must be ${expected}`);
  }
  return value;
}

function requireExactBoolean(value, label, expected) {
  if (value !== expected) {
    throw new Error(`Codex Android provider manifest ${label} must be ${String(expected)}`);
  }
  return value;
}

function requirePositiveInteger(value, label) {
  if (!Number.isInteger(value) || value <= 0) {
    throw new Error(`Codex Android provider manifest ${label} must be a positive integer`);
  }
  return value;
}

function requireCapabilityGroups(capabilities) {
  const groups = requireObject(capabilities, "environment.capabilities");
  const expectedGroupNames = Object.keys(ANDROID_PROVIDER_MANIFEST_DEFAULT_CAPABILITIES).sort();
  const actualGroupNames = Object.keys(groups).sort();
  if (JSON.stringify(actualGroupNames) !== JSON.stringify(expectedGroupNames)) {
    throw new Error("Codex Android provider manifest environment.capabilities groups must match the native Android contract");
  }

  for (const groupName of expectedGroupNames) {
    const expectedCapabilities = ANDROID_PROVIDER_MANIFEST_DEFAULT_CAPABILITIES[groupName];
    const actualCapabilities = requireArray(
      groups[groupName],
      `environment.capabilities.${groupName}`,
    );
    if (!arraysEqual(actualCapabilities, expectedCapabilities)) {
      throw new Error(`Codex Android provider manifest environment.capabilities.${groupName} must match the native Android contract`);
    }
  }

  return groups;
}

function requireOutcomeTaxonomy(outcomeTaxonomy) {
  const taxonomy = requireObject(outcomeTaxonomy, "policy.outcomeTaxonomy");
  const statuses = requireArray(taxonomy.statuses, "policy.outcomeTaxonomy.statuses");
  const retryability = requireArray(
    taxonomy.retryability,
    "policy.outcomeTaxonomy.retryability",
  );
  if (!arraysEqual(statuses, ANDROID_PROVIDER_MANIFEST_OUTCOME_TAXONOMY.statuses)) {
    throw new Error("Codex Android provider manifest policy.outcomeTaxonomy.statuses must match the native Android contract");
  }
  if (!arraysEqual(retryability, ANDROID_PROVIDER_MANIFEST_OUTCOME_TAXONOMY.retryability)) {
    throw new Error("Codex Android provider manifest policy.outcomeTaxonomy.retryability must match the native Android contract");
  }
  return taxonomy;
}

export function validateCodexAndroidProviderManifest(manifest) {
  const root = requireObject(manifest, "root");
  if (root.schemaVersion !== ANDROID_PROVIDER_MANIFEST_SCHEMA_VERSION) {
    throw new Error(
      `Codex Android provider manifest schemaVersion must be ${ANDROID_PROVIDER_MANIFEST_SCHEMA_VERSION}`,
    );
  }

  const provider = requireObject(root.provider, "provider");
  const environment = requireObject(root.environment, "environment");
  const policy = requireObject(root.policy, "policy");
  const leases = requireObject(policy.leases, "policy.leases");
  const observeLease = requireObject(leases.android_observe, "policy.leases.android_observe");
  const stepLease = requireObject(leases.android_step, "policy.leases.android_step");
  const installLease = requireObject(
    leases.android_install_build_from_run,
    "policy.leases.android_install_build_from_run",
  );
  const status = requireObject(root.status, "status");

  requireExactString(provider.family, "provider.family", "android");
  requireExactString(provider.adapter, "provider.adapter", "android");
  requireExactString(provider.transport, "provider.transport", "android-computer-use-mcp");
  requireExactString(environment.scope, "environment.scope", "environment");
  requireExactString(observeLease.mutationClass, "policy.leases.android_observe.mutationClass", "read_only");
  requireExactString(observeLease.leaseMode, "policy.leases.android_observe.leaseMode", "shared_read");
  requireExactString(stepLease.mutationClass, "policy.leases.android_step.mutationClass", "mutating");
  requireExactString(stepLease.leaseMode, "policy.leases.android_step.leaseMode", "exclusive_write");
  requireExactString(
    installLease.mutationClass,
    "policy.leases.android_install_build_from_run.mutationClass",
    "mutating",
  );
  requireExactString(
    installLease.leaseMode,
    "policy.leases.android_install_build_from_run.leaseMode",
    "exclusive_write",
  );
  requireExactString(policy.resumeBehavior, "policy.resumeBehavior", "revalidate_required");
  requireExactBoolean(policy.persistOnResume, "policy.persistOnResume", false);
  requireExactString(status.state, "status.state", "ready");

  const toolNames = requireToolNames(root);
  requireString(environment.mcpUrl, "environment.mcpUrl");
  const defaultPackageName = requireNullableString(
    environment.defaultPackageName,
    "environment.defaultPackageName",
  );
  const defaultActivity = requireNullableString(
    environment.defaultActivity,
    "environment.defaultActivity",
  );
  const artifactRoot = requireNullableString(environment.artifactRoot, "environment.artifactRoot");
  const buildManifestPath = requireNullableString(
    environment.buildManifestPath,
    "environment.buildManifestPath",
  );
  requireString(status.generatedAt, "status.generatedAt");
  const capabilities = requireCapabilityGroups(environment.capabilities);
  const outcomeTaxonomy = requireOutcomeTaxonomy(policy.outcomeTaxonomy);
  const timeoutMs = requirePositiveInteger(policy.timeoutMs, "policy.timeoutMs");

  return {
    ok: true,
    schemaVersion: root.schemaVersion,
    provider: {
      family: provider.family,
      adapter: provider.adapter,
      transport: provider.transport,
      toolNames,
    },
    environment: {
      scope: environment.scope,
      defaultPackageName,
      defaultActivity,
      artifactRoot,
      buildManifestPath,
      capabilityGroups: Object.keys(capabilities).sort(),
    },
    policy: {
      timeoutMs,
      observeLease,
      stepLease,
      installLease,
      resumeBehavior: policy.resumeBehavior ?? null,
      outcomeTaxonomy: {
        statuses: [...outcomeTaxonomy.statuses],
        retryability: [...outcomeTaxonomy.retryability],
      },
    },
    status: {
      state: status.state,
      generatedAt: status.generatedAt,
    },
  };
}
