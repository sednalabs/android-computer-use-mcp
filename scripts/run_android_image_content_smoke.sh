#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
artifact_dir="${RUNNER_TEMP:?RUNNER_TEMP must be set}/android-mcp-artifacts"
server_log="${RUNNER_TEMP}/android-computer-use-mcp.log"
mcp_binary="${MCP_BINARY:-${repo_dir}/target/debug/android-computer-use-mcp}"

mkdir -p "${artifact_dir}"
export ANDROID_COMPUTER_USE_MCP_SDK_ROOT="${ANDROID_SDK_ROOT:?ANDROID_SDK_ROOT must be set}"
export ANDROID_COMPUTER_USE_MCP_ARTIFACT_DIR="${artifact_dir}"
export ANDROID_BUILD_FINGERPRINT="$("${ANDROID_SDK_ROOT}/platform-tools/adb" -s "${ANDROID_SERIAL:-emulator-5554}" shell getprop ro.build.fingerprint | tr -d '\r\n')"
export ANDROID_EMULATOR_VERSION="$("${ANDROID_SDK_ROOT}/emulator/emulator" -version 2>/dev/null | sed -n '1p')"
export MCP_SOURCE_SHA="$(git -C "${repo_dir}" rev-parse HEAD)"
export MCP_BINARY_SHA256="$(sha256sum "${mcp_binary}" | cut -d ' ' -f 1)"
export ANDROID_MCP_JOURNEY_EVIDENCE="${ANDROID_MCP_JOURNEY_EVIDENCE:-${RUNNER_TEMP}/android-mcp-evidence-${MCP_PROTOCOL_MODE:-upstream-image}.json}"
# A failed rerun must not leave a previous success receipt at the selected path.
rm -f -- "${ANDROID_MCP_JOURNEY_EVIDENCE}"

if curl --connect-timeout 1 --max-time 2 -fsS "http://${ANDROID_COMPUTER_USE_MCP_BIND_ADDR:?ANDROID_COMPUTER_USE_MCP_BIND_ADDR must be set}/health" >/dev/null 2>&1; then
  echo "The fixture MCP port is already serving a health endpoint; use a clean test endpoint" >&2
  exit 1
fi

"${mcp_binary}" \
  --artifact-dir "${artifact_dir}" >"${server_log}" 2>&1 &
server_pid="$!"

cleanup() {
  if kill -0 "${server_pid}" >/dev/null 2>&1; then
    kill "${server_pid}" >/dev/null 2>&1 || true
    wait "${server_pid}" >/dev/null 2>&1 || true
  fi
}
trap cleanup 0

for attempt in $(seq 1 60); do
  if ! kill -0 "${server_pid}" >/dev/null 2>&1; then
    echo "MCP server exited before becoming healthy; inspect the local server log" >&2
    exit 1
  fi
  if curl --connect-timeout 1 --max-time 2 -fsS "http://${ANDROID_COMPUTER_USE_MCP_BIND_ADDR}/health" >/dev/null 2>&1; then
    break
  fi
  if [ "${attempt}" = "60" ]; then
    echo "MCP server did not become healthy" >&2
    exit 1
  fi
  sleep 2
done

if [ -n "${CODEX_BINARY:-}" ]; then
  node "${repo_dir}/scripts/codex_android_mcp_journey.mjs"
else
  npm exec --yes --package=@openai/codex@0.163.0 -- node "${repo_dir}/scripts/codex_android_mcp_journey.mjs"
fi
