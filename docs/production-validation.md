# Production validation

The release gates distinguish source tests, real Android execution and model
behaviour. Passing one gate does not imply that the others ran. Keep the exact
Android MCP commit, consumer commit and binary digests with each result.

## Automated gates

| Gate | What it establishes | What it does not establish |
| --- | --- | --- |
| Rust validation | Locked dependency build, formatting, Clippy, tool schemas and Rust regression tests | A real device journey |
| Adapter and consumer contracts | JavaScript contracts and negative tests for missing/corrupt images, wrong-device receipts and unsafe retry flags | Actual Codex or Android execution |
| Upstream image smoke | Pinned upstream Codex 0.163.0 discovers Android tools and forwards a real emulator PNG and JSON receipt | Sedna fork compatibility or action parity |
| Sedna Codex acceptance | Exact source-built fork discovers generic MCP tools; observe, successful action, duplicate ID and partial failure preserve typed PNGs and truthful receipts | Live model judgement, hosted build installation, multi-device races or a long session |

The consumer fixture drives a disposable local Responses endpoint with fixed
tool calls. It sends no real API credential and makes no call to a live model.
This tests what reaches the model-facing request, not whether a model correctly
understands the screen.

Image results must retain one JSON text receipt and one typed PNG. They must not
acquire `structuredContent` while a supported Codex consumer suppresses image
blocks when that field is present. The assertion walks PNG chunks, checks their
checksums, records dimensions and hashes the exact bytes.

The failure journey executes Back, attempts a selector that cannot match, and
places Home after it. It requires one completed action, two attempted calls,
`failed_index: 1`, a fresh failure screenshot and `do_not_replay: true`. Retrying
the successful batch with the same action ID must return
`duplicate_action_id: true`. Rust fake-provider tests are the stronger proof of
underlying dispatch counts; the consumer fixture checks the public receipt.

## Pinning the actual Sedna consumer

`scripts/sedna_codex_consumer_revision.txt` contains a full immutable commit ID.
The hosted workflow checks out that exact revision and builds `codex-cli` with
its declared Rust toolchain and locked dependencies. The build emits a receipt
binding the source revision to the executable's SHA-256. The journey rehashes
the executable before running it and rejects a mismatched receipt.

Update the pin deliberately after reviewing consumer changes. A passing run on
an earlier pin is not proof for a newer installed Codex executable.

The workflow runs both `protocol_mode = "legacy"` and
`protocol_mode = "2026-07-28"`. The latter permits consumer fallback when the
server does not implement the newer lifecycle. Evidence records the requested
preference; it does not claim that the newer protocol was negotiated.

To reproduce with an already checked-out pinned Codex source and a disposable
booted emulator:

```bash
bash scripts/build_sedna_codex_consumer.sh \
  <codex-checkout> <consumer-output-directory>

export CODEX_BINARY=<consumer-output-directory>/codex
export CODEX_BUILD_PROVENANCE=<consumer-output-directory>/provenance.json
export CODEX_SOURCE_SHA="$(cat scripts/sedna_codex_consumer_revision.txt)"
export ANDROID_SDK_ROOT=<android-sdk-directory>
export ANDROID_SERIAL=emulator-5554
export ANDROID_COMPUTER_USE_MCP_BIND_ADDR=127.0.0.1:9526
export RUNNER_TEMP=<temporary-test-directory>
sh scripts/run_sedna_codex_acceptance.sh
```

Use absolute output and SDK paths. Build the MCP with `cargo build --locked`
first, or set `MCP_BINARY` to the built executable. The runner refuses an already
healthy endpoint so an unrelated running server cannot satisfy the test. CI
preserves small JSON evidence receipts, without publishing screenshots, UI
hierarchies, arbitrary device logs or account content.

## Promotion evidence still required

Before declaring a deployment production-ready, collect successful results for
the actual deployed consumer/provider pair and its configuration, including:

- Hosted build installation with correct repository, run, APK and manifest
  digests; wrong identity and partial post-install failures must fail truthfully.
- A representative app journey including Unicode input, app lifecycle,
  orientation and atomic multi-touch on a backend that supports it.
- Two-device isolation and concurrent mutations; disconnects and dropped
  responses must not replay an uncertain operation.
- Long sessions and bounded artefact retention, with measured latency on the
  named host and emulator configuration.

The current consumer receipt explicitly records `hosted_build_install_tested:
false` and `live_model_judgement: false`. Those fields must only change when a
corresponding executed gate provides evidence. A workflow definition or queued
run is not a passing result.
