import test from "node:test";
import assert from "node:assert/strict";
import os from "node:os";
import path from "node:path";
import { access, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";

import {
  CF_ACCESS_CLIENT_ID_ENV_VAR,
  CF_ACCESS_CLIENT_SECRET_ENV_VAR,
  LEGACY_CF_ACCESS_CLIENT_ID_ENV_VAR,
  LEGACY_CF_ACCESS_CLIENT_SECRET_ENV_VAR,
  defaultCodexAdapterConfigPath,
  legacyCodexAdapterConfigPath,
  loadCodexAdapterConfig,
  requiredOptionValue,
  resolveMcpHeaders,
  resolveMcpUrl,
} from "./cli_common.js";

test("requiredOptionValue rejects missing values without consuming the next option", () => {
  assert.throws(
    () => requiredOptionValue(["--config", "--mcp-url", "https://android.example/mcp"], 0),
    /Option --config requires a non-option value/,
  );
  assert.throws(
    () => requiredOptionValue(["--config"], 0),
    /Option --config requires a non-option value/,
  );
  assert.equal(requiredOptionValue(["--config", "config.json"], 0), "config.json");
});

test("defaultCodexAdapterConfigPath points at the standard Codex config location", () => {
  assert.equal(
    defaultCodexAdapterConfigPath("/tmp/home"),
    "/tmp/home/.codex/android-dynamic-tools.json",
  );
  assert.equal(
    legacyCodexAdapterConfigPath("/tmp/home"),
    "/tmp/home/.codex/solarlab-android-dynamic-tools.json",
  );
});

test("loadCodexAdapterConfig uses the default config path when present", async () => {
  const tempHome = await mkdtemp(path.join(os.tmpdir(), "codex-cli-common-"));
  try {
    const configPath = defaultCodexAdapterConfigPath(tempHome);
    await mkdir(path.dirname(configPath), { recursive: true });
    await writeFile(
      configPath,
      JSON.stringify({
        mcp_url: "https://solarlab-android-mcp.sednalabs.io/mcp",
        default_serial: "emulator-5554",
      }),
    );

    const config = await loadCodexAdapterConfig({
      homeDir: tempHome,
      access,
    });

    assert.equal(config.mcp_url, "https://solarlab-android-mcp.sednalabs.io/mcp");
    assert.equal(config.default_serial, "emulator-5554");
  } finally {
    await rm(tempHome, { recursive: true, force: true });
  }
});

test("loadCodexAdapterConfig returns an empty object when the default config is absent", async () => {
  const tempHome = await mkdtemp(path.join(os.tmpdir(), "codex-cli-common-"));
  try {
    const config = await loadCodexAdapterConfig({
      homeDir: tempHome,
      access,
    });
    assert.deepEqual(config, {});
  } finally {
    await rm(tempHome, { recursive: true, force: true });
  }
});

test("loadCodexAdapterConfig throws when an explicit config path is missing", async () => {
  await assert.rejects(
    loadCodexAdapterConfig({
      configPath: "/tmp/definitely-missing-solarlab-codex-config.json",
      access,
    }),
    /ENOENT/,
  );
});

test("resolveMcpUrl prefers explicit URL, then config, then environment", () => {
  assert.equal(
    resolveMcpUrl({
      explicitUrl: "https://explicit.example/mcp",
      config: { mcp_url: "https://config.example/mcp" },
      env: {
        SOLARLAB_ANDROID_MCP_URL: "https://env.example/mcp",
        CODEX_ANDROID_MCP_URL: "https://generic-env.example/mcp",
        SOLARLAB_ANDROID_MCP_HOSTNAME: "hostname.example",
      },
    }),
    "https://explicit.example/mcp",
  );

  assert.equal(
    resolveMcpUrl({
      config: { mcp_url: "https://config.example/mcp" },
      env: {
        SOLARLAB_ANDROID_MCP_URL: "https://env.example/mcp",
        SOLARLAB_ANDROID_MCP_HOSTNAME: "hostname.example",
      },
    }),
    "https://config.example/mcp",
  );

  assert.equal(
    resolveMcpUrl({
      env: {
        SOLARLAB_ANDROID_MCP_URL: "https://env.example/mcp",
        CODEX_ANDROID_MCP_URL: "https://generic-env.example/mcp",
        SOLARLAB_ANDROID_MCP_HOSTNAME: "hostname.example",
      },
    }),
    "https://generic-env.example/mcp",
  );

  assert.equal(
    resolveMcpUrl({
      env: {
        CODEX_ANDROID_MCP_HOSTNAME: "generic-hostname.example",
        SOLARLAB_ANDROID_MCP_HOSTNAME: "hostname.example",
      },
    }),
    "https://generic-hostname.example/mcp",
  );
});

test("resolveMcpUrl still accepts legacy Solar Lab environment aliases", () => {
  assert.equal(
    resolveMcpUrl({
      env: {
        SOLARLAB_ANDROID_MCP_URL: "https://env.example/mcp",
      },
    }),
    "https://env.example/mcp",
  );

  assert.equal(
    resolveMcpUrl({
      env: {
        SOLARLAB_ANDROID_MCP_HOSTNAME: "hostname.example",
      },
    }),
    "https://hostname.example/mcp",
  );
});

test("resolveMcpHeaders merges env-backed CF Access headers with explicit headers", () => {
  const headers = resolveMcpHeaders({
    headerArgs: ["X-Debug=1"],
    env: {
      [CF_ACCESS_CLIENT_ID_ENV_VAR]: "client-id",
      [CF_ACCESS_CLIENT_SECRET_ENV_VAR]: "client-secret",
    },
  });

  assert.deepEqual(headers, {
    "CF-Access-Client-Id": "client-id",
    "CF-Access-Client-Secret": "client-secret",
    "X-Debug": "1",
  });
});

test("resolveMcpHeaders accepts legacy Solar Lab CF Access aliases", () => {
  const headers = resolveMcpHeaders({
    env: {
      [LEGACY_CF_ACCESS_CLIENT_ID_ENV_VAR]: "client-id",
      [LEGACY_CF_ACCESS_CLIENT_SECRET_ENV_VAR]: "client-secret",
    },
  });

  assert.deepEqual(headers, {
    "CF-Access-Client-Id": "client-id",
    "CF-Access-Client-Secret": "client-secret",
  });
});

test("resolveMcpHeaders rejects partial CF Access environment configuration", () => {
  assert.throws(
    () =>
      resolveMcpHeaders({
        env: {
          [CF_ACCESS_CLIENT_ID_ENV_VAR]: "client-id",
        },
      }),
    /must be set together/,
  );
});
