# Disposable Android acceptance app

This offline APK gives the real-emulator gate stable controls and observable
effects. It requests no permissions and makes no network requests. The touch pad
reports the maximum simultaneous pointer count for each `MotionEvent` gesture.
Its accessibility description counts completed two-pointer gestures only when
both pointers remain inside separate, asymmetric target regions on the pad until
release. The runner requires a new completed gesture in landscape, then another
in portrait; neither independent taps nor the previous orientation's result can
satisfy the next check. Controls and the complete pad remain visible side by side
in landscape, preserving the counter and entered text across orientation changes.
On launch, the fixture writes a fixed nonsecret clipboard sentinel. Its dialog
reports only whether that sentinel is preserved, allowing the gate to verify
clipboard restoration after exact Unicode entry without exposing clipboard text.

The builder uses Android platform 35, build tools 35.0.0, a Java compiler and
standard ZIP tools. It creates a fresh signing key for each run. The APK is for a
new disposable emulator, and is not an application release or an upgrade test.

```bash
export ANDROID_ACCEPTANCE_ARTIFACT_NAME="android-acceptance-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}"
bash scripts/build_android_acceptance_fixture.sh "$RUNNER_TEMP/android-acceptance-build"
```

The builder writes `acceptance.apk` and `interactive-build-manifest.json` to its
`bundle` output directory. When `GITHUB_OUTPUT` exists, it exports `artifact_name`,
`source_sha`, `bundle_dir`, `apk_sha256`, `package_name` and `activity_name`.
Upload that directory using the workflow's pinned `actions/upload-artifact`.
Pass its exact archive digest to the runner as
`ANDROID_ACCEPTANCE_ARTIFACT_SHA256`; this is distinct from the APK digest.

The provider runner also needs `ANDROID_ACCEPTANCE_ARTIFACT_NAME`,
`ANDROID_ACCEPTANCE_SOURCE_SHA`, `GITHUB_REPOSITORY`, `GITHUB_RUN_ID`,
`ANDROID_SDK_ROOT`, `ANDROID_SERIAL` and `RUNNER_TEMP`. Supply a GitHub token with
read access to this run's artifacts through
`ANDROID_COMPUTER_USE_MCP_INTERACTIVE_SESSION_GITHUB_TOKEN`. It is passed to the
provider as environment configuration and never included in a tool request or
evidence receipt. `MCP_BINARY`, `MCP_ENDPOINT` and `ANDROID_PROVIDER_EVIDENCE` are
optional overrides.

`scripts/android_provider_acceptance.mjs` owns a fresh loopback server and refuses
an occupied endpoint or an already installed fixture. It checks provider identity
rejection, archive-digest rejection, exact installed APK bytes, install-only
behaviour, durable duplicate IDs, semantic taps, exact ASCII entry and Unicode replacement, clipboard
restoration, dialogs, scrolling, orientation, a native-coordinate landscape tap,
genuine simultaneous touch in landscape and portrait,
lifecycle and partial batch failure.
The emulator must publish a gRPC endpoint bound to its serial. No synthetic
discovery metadata or ADB approximation is accepted for the multi-touch check.
The runner requires a disposable emulator without secure credentials, wakes it,
keeps it awake while plugged in, and requests normal keyguard dismissal before
the UI journey. It records bounded power and keyguard state to diagnose readiness.

The same hosted bundle is reusable by the pinned Sedna Codex consumer gate. Resolve
the current target dynamically and attach `expected_build` containing the exact
repository, run ID, source commit, artifact name and archive digest. Successful
workflow definitions alone do not establish a passing run: evidence is written
only after the runner executes, and any failed check marks the receipt as failed.

Evidence contains timings, image dimensions and hashes, build provenance and
check outcomes. Before and after rotation, bounded geometry summaries retain
the logical and physical display frames, active touch viewport, raw X/Y axes and
affine coefficients from `wm size`, `dumpsys input` and `dumpsys display`. The
summaries omit display identifiers, device names, window titles and other settings.
Failed calls also include bounded hierarchy summaries with
package names, classes, resource IDs and allowlisted static fixture labels.
It excludes screenshots, full UI dumps, arbitrary device logs and
credentials. This is a representative scripted journey, not a long-duration soak,
multi-device isolation test or live-model judgement evaluation.
