# Android agent workflows

Use the generic MCP connection in Codex. Android execution stays in this server;
no retired native Android provider or Codex-specific tool bridge is required.
Start with `android.resolve_target`, `android.observe`, `android.step` and
`android.get_action_status`. Enable `android.install_build_from_run` when the
hosted build configuration is present.

## Observe and act

`android.observe` takes an optional exact `serial` and provider `target`, plus
`timeout_secs` from 1 to 15. It returns one typed PNG and one JSON text receipt.
If the UI hierarchy is unavailable, screenshot fallback is labelled explicitly.
If pixels are unavailable, the result is an MCP error rather than visual success.
Provider identity and device constraints apply to both observe and step.
`target.expected_build` is accepted only by the hosted install and relaunch
contracts; observe and step reject it rather than imply that they verified the
installed build.

```json
{"serial":"emulator-5554"}
```

`android.step` accepts one to sixteen actions using the documented low-level tool
arguments. All argument shapes and target serials are checked before dispatch.
The server holds a device lease through the final observation, executes actions
serially and stops at the first failure or cancellation. A two-minute dispatch
budget stops further actions; an already dispatched action finishes its bounded
execution and cleanup before the final observation. A batch is not a
transaction: completed actions are not rolled back.

App launch, semantic tap/type and raw input use a 30-second default budget for
dispatch and verification. This allows the initial UI hierarchy read and repeated
confirmation reads to finish on a real emulator. Set an action's `timeout_secs`
explicitly when a different budget is needed; `android.step` accepts 1 through 90
seconds per action. The two-minute batch dispatch budget still prevents starting
further actions after it expires, and increasing a deadline never enables
automatic mutation retries.

```json
{
  "serial": "emulator-5554",
  "action_id": "journey-2026-11-19-open-settings",
  "actions": [
    {"tool":"android.input.keyevent","arguments":{"keycode":"KEYCODE_HOME"}},
    {"tool":"android.launch_app","arguments":{"package_name":"com.android.settings"}}
  ]
}
```

The batch permits semantic tap/type/scroll, raw input, app launch/termination,
HTTP(S) URL opening and orientation changes. Installation, uninstall, nested
batches and arbitrary tools are excluded. See the schema for supported names.
Direct low-level tools remain available when their specific behaviour is needed;
they do not acquire the workflow's durable action-ID guarantee.

Coordinate gestures must be the first action and supply `observation_id` from a
recent `android.observe`. The server checks the observation's device, boot ID,
30-second age, window identity, orientation, dimensions and bounds. Animation
within the same window is allowed. Set `strict_frame_match: true` to require
identical pixels. Prefer semantic selectors where possible. Every batch
checks the boot identity again before each dispatch and stops if it changes.

## Interpret outcomes and recover

The receipt includes `ok`, `outcome`, `executed_action_count`,
`attempted_action_count`, zero-based `failed_index`, per-action receipts,
`retry_safe`, `do_not_replay`, and the final observation.

`executed_action_count` counts successfully completed action calls. A failed
call may still have affected the device; `attempted_action_count` includes that
call. `dispatched` confirms transport completion. `verified` additionally needs
an explicit satisfied postcondition. An empty postcondition is not UI proof.

Each caller-generated action ID is bound to its request and exact device.
Intent is flushed to disk before dispatch, and completion is saved before
screenshot capture. A repeated ID returns the prior receipt and a fresh
observation with `duplicate_action_id: true`; it never repeats the mutation.
A cancelled, timed-out or interrupted operation remains `outcome_unknown` until
reconciled. There is no automatic resume of a partly completed batch.

After a lost response, provider restart or device disconnect, call
`android.get_action_status` with the same `action_id`. This does not touch the
device and works when it is offline. Then observe and decide the next action.
A missing receipt is not proof that an operation never happened. Do not assign a
new ID merely to replay an uncertain tap, text entry or installation.

Receipts live in the private `.action-ledger` under the configured artifact root.
The artifact reader cannot expose that directory. Pending and complete IDs are
not silently evicted. The ledger stops accepting new IDs at its capacity;
archive it deliberately when retiring a session, while retaining evidence for
any uncertain operations. Deleting or changing the artifact root discards its
recovery history and must not be treated as permission to replay old requests.

## Install a hosted build

`android.install_build_from_run` takes `action_id` and a `build` object using the
hosted install contract. Prefer the versioned request:

```json
{
  "action_id": "install-run-123456",
  "build": {
    "contract_version": "android-provider-execution/v1",
    "workflow_run_id": 123456,
    "artifact_name": "interactive-build",
    "repository": "example/app",
    "install": {"launch_after_install": false},
    "target": {
      "environment_id": "runner-a",
      "provider_instance_id": "android-a",
      "provider_epoch": "<epoch returned by resolve_target>",
      "session_id": "session-a",
      "device_serial": "emulator-5554",
      "expected_build": {
        "repository": "example/app",
        "commit_sha": "<40-character commit SHA>",
        "workflow_run_id": 123456,
        "artifact_name": "interactive-build",
        "artifact_sha256": "sha256:<64-character artifact archive digest>"
      }
    }
  }
}
```

Replace placeholders with the actual provider receipt and build provenance.
The repository must match the operator's hosted-session configuration. The
server verifies GitHub run identity, archive and manifest digests, package and
activity, APK bytes, and installed base APK identity. An explicit expected
commit supports workflows building a selected checkout; legacy requests require
the workflow head. The current contract installs a single APK, not split APK sets.

Install-only requests neither launch nor stop the application. Signing conflicts
fail without uninstalling or deleting app data. If the operator deliberately
needs a clean install, uninstall is a separate explicit operation. A successful
installation followed by launch, persistence or observation failure retains its
known installation effects in the receipt.

The legacy `interactive_session.install_build_from_run` remains available with
its translated argument layout and phase receipts. Use the new wrapper to gain
a durable action ID and a post-install image. A provider restart invalidates
persisted active-build reuse; revalidate the installed app rather than treating
an old provider epoch as current.

## Operating limits

Run one server process for each configured SDK/artifact/session scope. All MCP
sessions in that process share device leases. These leases do not coordinate
unrelated ADB clients or other server processes. Use a dedicated emulator and
operator-owned directories; this remains trusted local automation.

Set distinct `ANDROID_COMPUTER_USE_MCP_ENVIRONMENT_ID`,
`ANDROID_COMPUTER_USE_MCP_PROVIDER_INSTANCE_ID` and
`ANDROID_COMPUTER_USE_MCP_SESSION_ID` for distinct hosted sessions. The process
provides its own fresh epoch. The HTTP health endpoint reports readiness and
session counts without disclosing SDK or artifact paths.

Generated artifact writes stop at a 1 GiB directory budget, 4096 files or a
32 MiB individual file. Existing artifacts are never silently removed. The
private action ledger has its own 10,000-record limit. Provision a persistent,
private artifact directory and archive completed sessions explicitly. Emulator
stdout log streams are managed by the host's logging/rotation policy.

Atomic multi-touch requires a serial-bound emulator gRPC endpoint. Unsupported
backends refuse the gesture. A gRPC mutation failure does not trigger an ADB
replay. ADB text input rejects characters it cannot preserve; use an appropriate
gRPC-capable emulator for Unicode and control-character input.

See [Production validation](production-validation.md) for the executed evidence
required before promoting a particular consumer/provider pair.
