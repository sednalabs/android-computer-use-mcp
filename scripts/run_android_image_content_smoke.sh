#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
artifact_dir="${RUNNER_TEMP:?RUNNER_TEMP must be set}/android-mcp-artifacts"
summary_dir="${repo_dir}/dist/android-image-content-smoke"
server_log="${RUNNER_TEMP}/android-computer-use-mcp.log"

mkdir -p "${artifact_dir}" "${summary_dir}"

export ANDROID_COMPUTER_USE_MCP_SDK_ROOT="${ANDROID_SDK_ROOT:?ANDROID_SDK_ROOT must be set}"
export ANDROID_COMPUTER_USE_MCP_ARTIFACT_DIR="${artifact_dir}"
export ANDROID_IMAGE_CONTENT_SUMMARY="${summary_dir}/summary.json"

"${repo_dir}/target/debug/android-computer-use-mcp" \
  --artifact-dir "${artifact_dir}" >"${server_log}" 2>&1 &
server_pid="$!"

cleanup() {
  if kill -0 "${server_pid}" >/dev/null 2>&1; then
    kill "${server_pid}" >/dev/null 2>&1 || true
    wait "${server_pid}" >/dev/null 2>&1 || true
  fi
  echo "### android-computer-use-mcp log"
  tail -n 200 "${server_log}" || true
}
trap cleanup 0

for attempt in $(seq 1 60); do
  if curl -fsS "http://${ANDROID_COMPUTER_USE_MCP_BIND_ADDR:?ANDROID_COMPUTER_USE_MCP_BIND_ADDR must be set}/health" >/dev/null; then
    break
  fi
  if [ "${attempt}" = "60" ]; then
    echo "MCP server did not become healthy" >&2
    exit 1
  fi
  sleep 2
done

node "${repo_dir}/scripts/android_image_content_smoke.mjs"
