#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
: "${CODEX_BINARY:?CODEX_BINARY must name the exact source-built Sedna Codex executable}"
: "${CODEX_SOURCE_SHA:?CODEX_SOURCE_SHA must be set}"
: "${CODEX_BUILD_PROVENANCE:?CODEX_BUILD_PROVENANCE must be set}"
export ANDROID_MCP_JOURNEY=production
for mode in legacy 2026-07-28; do
  MCP_PROTOCOL_MODE="${mode}" sh "${repo_dir}/scripts/run_android_image_content_smoke.sh"
done
