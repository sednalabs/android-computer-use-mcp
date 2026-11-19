#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
source_dir=${1:?Usage: build_sedna_codex_consumer.sh SOURCE_DIR OUTPUT_DIR}
output_dir=${2:?Usage: build_sedna_codex_consumer.sh SOURCE_DIR OUTPUT_DIR}
source_dir=$(CDPATH= cd -- "${source_dir}" && pwd)
mkdir -p "${output_dir}"
output_dir=$(CDPATH= cd -- "${output_dir}" && pwd)
expected_sha=$(cat "${repo_dir}/scripts/sedna_codex_consumer_revision.txt")
actual_sha=$(git -C "${source_dir}" rev-parse HEAD)
[[ "${expected_sha}" =~ ^[a-f0-9]{40}$ ]]
[[ "${actual_sha}" == "${expected_sha}" ]] || {
  echo 'Codex checkout does not match the pinned source revision' >&2
  exit 1
}
[[ -z "$(git -C "${source_dir}" status --porcelain --untracked-files=normal)" ]] || {
  echo 'Codex checkout must be clean before building the acceptance consumer' >&2
  exit 1
}

toolchain=$(python3 - "${source_dir}/codex-rs/rust-toolchain.toml" <<'PY'
import sys, tomllib
with open(sys.argv[1], 'rb') as source:
    print(tomllib.load(source)['toolchain']['channel'])
PY
)
rustup toolchain install "${toolchain}" --profile minimal --no-self-update

# Follow the pinned consumer's official V8 artifact setup. Verify its published
# checksum list before Cargo receives the archive and generated bindings.
v8_version=$(python3 "${source_dir}/.github/scripts/rusty_v8_bazel.py" resolved-v8-crate-version)
v8_dir="${output_dir}/rusty-v8"
mkdir -p "${v8_dir}"
target=x86_64-unknown-linux-gnu
v8_base="https://github.com/openai/codex/releases/download/rusty-v8-v${v8_version}"
for name in "librusty_v8_release_${target}.a.gz" "src_binding_release_${target}.rs" "rusty_v8_release_${target}.sha256"; do
  curl --fail --silent --show-error --location --retry 3 "${v8_base}/${name}" --output "${v8_dir}/${name}"
done
[[ "$(wc -l < "${v8_dir}/rusty_v8_release_${target}.sha256")" -eq 2 ]]
(
  cd "${v8_dir}"
  sha256sum --check "rusty_v8_release_${target}.sha256"
)

export RUSTY_V8_ARCHIVE="${v8_dir}/librusty_v8_release_${target}.a.gz"
export RUSTY_V8_SRC_BINDING_PATH="${v8_dir}/src_binding_release_${target}.rs"
export CARGO_TARGET_DIR="${output_dir}/build"
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_INCREMENTAL=0
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
export CARGO_NET_GIT_FETCH_WITH_CLI=true
(
  cd "${source_dir}/codex-rs"
  cargo "+${toolchain}" build --locked --package codex-cli --bin codex
)
cp "${CARGO_TARGET_DIR}/debug/codex" "${output_dir}/codex"

python3 - "${output_dir}" "${actual_sha}" "${toolchain}" <<'PY'
import hashlib, json, pathlib, sys
root, source_sha, toolchain = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3]
receipt = {
    'schema_version': 1,
    'repository': 'sednalabs/codex',
    'source_sha': source_sha,
    'binary_sha256': hashlib.file_digest((root / 'codex').open('rb'), 'sha256').hexdigest(),
    'rust_toolchain': toolchain,
    'build_command': 'cargo build --locked --package codex-cli --bin codex',
    'profile': 'dev, debug=0, incremental=0',
}
(root / 'provenance.json').write_text(json.dumps(receipt, indent=2) + '\n')
PY
