#!/usr/bin/env bash
# Build an offline, disposable APK with SDK tools only. Output is a hosted-install bundle.
set -euo pipefail

repo_dir=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
output_dir=${1:?Usage: build_android_acceptance_fixture.sh absolute-output-directory}
[[ "$output_dir" = /* ]] || { echo "Output directory must be absolute" >&2; exit 1; }
sdk_root=${ANDROID_SDK_ROOT:-${ANDROID_HOME:?Android SDK must be configured}}
build_tools="$sdk_root/build-tools/35.0.0"
android_jar="$sdk_root/platforms/android-35/android.jar"
artifact_name=${ANDROID_ACCEPTANCE_ARTIFACT_NAME:?Set ANDROID_ACCEPTANCE_ARTIFACT_NAME}
[[ "$artifact_name" =~ ^[A-Za-z0-9._-]+$ ]]
repository=${GITHUB_REPOSITORY:?Set GITHUB_REPOSITORY}
run_id=${GITHUB_RUN_ID:?Set GITHUB_RUN_ID}
run_attempt=${GITHUB_RUN_ATTEMPT:?Set GITHUB_RUN_ATTEMPT}
source_sha=$(git -C "$repo_dir" rev-parse HEAD)
[[ "$source_sha" =~ ^[a-f0-9]{40}$ ]]
[[ "$run_id" =~ ^[1-9][0-9]*$ && "$run_attempt" =~ ^[1-9][0-9]*$ ]]
test -f "$android_jar"
test -x "$build_tools/aapt2"
mkdir -p "$output_dir"
work_dir=$(mktemp -d "$output_dir/.build-XXXXXX")
trap 'rm -rf -- "$work_dir"' EXIT
bundle_dir="$output_dir/bundle"
mkdir -p "$bundle_dir" "$work_dir/generated" "$work_dir/classes" "$work_dir/dex"
test -z "$(find "$bundle_dir" -mindepth 1 -maxdepth 1 -print -quit)" || {
  echo "Refusing to mix fixture output with an existing bundle" >&2; exit 1;
}

"$build_tools/aapt2" compile --dir "$repo_dir/tests/android-fixture/res" -o "$work_dir/resources.zip"
"$build_tools/aapt2" link -o "$work_dir/unsigned.apk" \
  -I "$android_jar" --manifest "$repo_dir/tests/android-fixture/AndroidManifest.xml" \
  --java "$work_dir/generated" --min-sdk-version 23 --target-sdk-version 35 "$work_dir/resources.zip"
javac -source 8 -target 8 -bootclasspath "$android_jar" -d "$work_dir/classes" \
  "$work_dir/generated/com/sednalabs/mcp/acceptance/R.java" \
  "$repo_dir/tests/android-fixture/src/com/sednalabs/mcp/acceptance/MainActivity.java"
jar cf "$work_dir/classes.jar" -C "$work_dir/classes" .
"$build_tools/d8" --min-api 23 --lib "$android_jar" --output "$work_dir/dex" "$work_dir/classes.jar"
(cd "$work_dir/dex" && zip -q "$work_dir/unsigned.apk" classes.dex)
"$build_tools/zipalign" -p -f 4 "$work_dir/unsigned.apk" "$work_dir/aligned.apk"
# This key signs only this run's disposable fixture; it is never an application release key.
keytool -genkeypair -noprompt -keystore "$work_dir/fixture.p12" -storetype PKCS12 \
  -alias fixture -storepass fixture-only -keypass fixture-only -keyalg RSA -keysize 2048 \
  -validity 2 -dname "CN=Disposable Android MCP Acceptance" >/dev/null 2>&1
"$build_tools/apksigner" sign --ks "$work_dir/fixture.p12" --ks-key-alias fixture \
  --ks-pass pass:fixture-only --key-pass pass:fixture-only \
  --out "$bundle_dir/acceptance.apk" "$work_dir/aligned.apk"
"$build_tools/apksigner" verify "$bundle_dir/acceptance.apk"

export FIXTURE_BUNDLE_DIR="$bundle_dir" FIXTURE_ARTIFACT_NAME="$artifact_name"
export FIXTURE_REPOSITORY="$repository" FIXTURE_RUN_ID="$run_id" FIXTURE_RUN_ATTEMPT="$run_attempt"
export FIXTURE_SOURCE_SHA="$source_sha"
python3 - <<'PY'
import datetime, hashlib, json, os
from pathlib import Path

bundle = Path(os.environ['FIXTURE_BUNDLE_DIR'])
apk_sha256 = hashlib.sha256((bundle / 'acceptance.apk').read_bytes()).hexdigest()
manifest = {
    'schema_version': 1,
    'artifact_name': os.environ['FIXTURE_ARTIFACT_NAME'],
    'repository': os.environ['FIXTURE_REPOSITORY'],
    'workflow': os.environ.get('GITHUB_WORKFLOW', 'Android provider acceptance'),
    'run_id': os.environ['FIXTURE_RUN_ID'],
    'run_attempt': os.environ['FIXTURE_RUN_ATTEMPT'],
    'checkout_ref': os.environ['FIXTURE_SOURCE_SHA'],
    'commit_sha': os.environ['FIXTURE_SOURCE_SHA'],
    'android_validation_mode': 'real-emulator-acceptance',
    'interactive_debug_profile': 'offline-disposable-fixture',
    'package_name': 'com.sednalabs.mcp.acceptance',
    'activity_name': 'com.sednalabs.mcp.acceptance.MainActivity',
    'version_name': '1.0',
    'apk_filename': 'acceptance.apk',
    'apk_sha256': apk_sha256,
    'built_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
}
(bundle / 'interactive-build-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
outputs = {
    'artifact_name': manifest['artifact_name'], 'source_sha': manifest['commit_sha'],
    'bundle_dir': str(bundle), 'apk_sha256': apk_sha256,
    'package_name': manifest['package_name'], 'activity_name': manifest['activity_name'],
}
if os.environ.get('GITHUB_OUTPUT'):
    with open(os.environ['GITHUB_OUTPUT'], 'a') as stream:
        for key, value in outputs.items():
            stream.write(f'{key}={value}\n')
print(json.dumps(outputs))
PY
