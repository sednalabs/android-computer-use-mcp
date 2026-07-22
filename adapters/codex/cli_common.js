import os from "node:os";
import path from "node:path";
import { promises as fs } from "node:fs";

export const DEFAULT_CONFIG_ENV_VAR = "CODEX_ANDROID_DYNAMIC_TOOLS_CONFIG";
export const LEGACY_DEFAULT_CONFIG_ENV_VAR = "SOLARLAB_ANDROID_DYNAMIC_TOOLS_CONFIG";
export const MCP_URL_ENV_VAR = "CODEX_ANDROID_MCP_URL";
export const LEGACY_MCP_URL_ENV_VAR = "SOLARLAB_ANDROID_MCP_URL";
export const MCP_HOSTNAME_ENV_VAR = "CODEX_ANDROID_MCP_HOSTNAME";
export const LEGACY_MCP_HOSTNAME_ENV_VAR = "SOLARLAB_ANDROID_MCP_HOSTNAME";
export const CF_ACCESS_CLIENT_ID_ENV_VAR = "CODEX_ANDROID_MCP_CF_ACCESS_CLIENT_ID";
export const LEGACY_CF_ACCESS_CLIENT_ID_ENV_VAR =
  "SOLARLAB_ANDROID_MCP_CF_ACCESS_CLIENT_ID";
export const CF_ACCESS_CLIENT_SECRET_ENV_VAR = "CODEX_ANDROID_MCP_CF_ACCESS_CLIENT_SECRET";
export const LEGACY_CF_ACCESS_CLIENT_SECRET_ENV_VAR =
  "SOLARLAB_ANDROID_MCP_CF_ACCESS_CLIENT_SECRET";
export const LEGACY_CONFIG_FILENAME = "solarlab-android-dynamic-tools.json";

export function defaultCodexAdapterConfigPath(homeDir = os.homedir()) {
  return path.join(homeDir, ".codex", "android-dynamic-tools.json");
}

export function legacyCodexAdapterConfigPath(homeDir = os.homedir()) {
  return path.join(homeDir, ".codex", LEGACY_CONFIG_FILENAME);
}

export function parseHeaderArgs(headerArgs = []) {
  const headers = {};
  for (const headerArg of headerArgs) {
    const separator = headerArg.indexOf("=");
    if (separator <= 0) {
      throw new Error(`Invalid --mcp-header value: ${headerArg}`);
    }
    headers[headerArg.slice(0, separator)] = headerArg.slice(separator + 1);
  }
  return headers;
}

function accessHeadersFromEnv(env) {
  const clientId =
    env[CF_ACCESS_CLIENT_ID_ENV_VAR] ?? env[LEGACY_CF_ACCESS_CLIENT_ID_ENV_VAR];
  const clientSecret =
    env[CF_ACCESS_CLIENT_SECRET_ENV_VAR] ?? env[LEGACY_CF_ACCESS_CLIENT_SECRET_ENV_VAR];

  if (!clientId && !clientSecret) {
    return {};
  }

  if (!clientId || !clientSecret) {
    throw new Error(
      `${CF_ACCESS_CLIENT_ID_ENV_VAR} and ${CF_ACCESS_CLIENT_SECRET_ENV_VAR} must be set together`,
    );
  }

  return {
    "CF-Access-Client-Id": clientId,
    "CF-Access-Client-Secret": clientSecret,
  };
}

export function resolveMcpHeaders({
  headerArgs = [],
  env = process.env,
} = {}) {
  return {
    ...accessHeadersFromEnv(env),
    ...parseHeaderArgs(headerArgs),
  };
}

export function resolveMcpUrl({
  explicitUrl,
  config = {},
  env = process.env,
} = {}) {
  const hostname = env[MCP_HOSTNAME_ENV_VAR] ?? env[LEGACY_MCP_HOSTNAME_ENV_VAR];
  return (
    explicitUrl ??
    config.mcp_url ??
    env[MCP_URL_ENV_VAR] ??
    env[LEGACY_MCP_URL_ENV_VAR] ??
    (hostname ? `https://${hostname}/mcp` : null)
  );
}

export async function loadCodexAdapterConfig({
  configPath,
  env = process.env,
  homeDir = os.homedir(),
  readFile = fs.readFile,
  access = fs.access,
} = {}) {
  const envConfigPath =
    env[DEFAULT_CONFIG_ENV_VAR] ?? env[LEGACY_DEFAULT_CONFIG_ENV_VAR] ?? null;
  const defaultConfigPath = defaultCodexAdapterConfigPath(homeDir);
  const resolvedPath = configPath ?? envConfigPath ?? defaultConfigPath;
  const required = Boolean(configPath ?? envConfigPath);

  try {
    await access(resolvedPath);
  } catch (error) {
    if (!required && error?.code === "ENOENT") {
      const legacyPath = legacyCodexAdapterConfigPath(homeDir);
      try {
        await access(legacyPath);
      } catch (legacyError) {
        if (legacyError?.code === "ENOENT") {
          return {};
        }
        throw legacyError;
      }
      return JSON.parse(await readFile(legacyPath, "utf8"));
    }
    throw error;
  }

  return JSON.parse(await readFile(resolvedPath, "utf8"));
}
