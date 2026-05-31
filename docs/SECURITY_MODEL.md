# Security Model

`android-computer-use-mcp` is a trusted local automation server. It is useful because
it can control emulators, install APKs, drive input, and collect artifacts. Those
same capabilities are sensitive and should stay inside a controlled local or
runner-owned environment.

## Trust Boundary

The trusted boundary is:

- the local operator or hosted runner that starts the MCP server
- the configured Android SDK tools
- the configured artifact directory
- loopback clients that the operator intentionally connects

The server is not designed to accept arbitrary internet traffic, untrusted
multi-tenant callers, or unreviewed APKs from unknown sources.

## Network Boundary

The Streamable HTTP runtime defaults to `127.0.0.1:9526` and configuration
validation rejects non-loopback bind addresses. Keep it that way unless the code
has been intentionally hardened for a broader deployment model.

`ANDROID_COMPUTER_USE_MCP_ALLOWED_HOSTS` should contain only loopback hostnames or
addresses for the current local deployment.

## Process Boundary

The server shells out to configured Android SDK binaries:

- `adb`
- `emulator`
- `avdmanager`

Configure those paths explicitly or through the SDK root. Do not point them at
wrapper scripts or binaries from untrusted directories.

## Artifact Boundary

The server writes screenshots, UI XML, logcat, scenario bundles, and related
proof files under `ANDROID_COMPUTER_USE_MCP_ARTIFACT_DIR`.

Treat those artifacts as potentially sensitive because they can contain:

- app UI content
- user-entered text
- package names and activity names
- device state
- logs or crash output
- file paths from the runner

Do not commit generated artifacts unless they have been reviewed for public
release.

## APK and App Boundary

Only install APKs that are expected for the current task. An APK installed into
an emulator can still exercise Android permissions, write app data, produce logs,
or expose sensitive UI in screenshots.

Hosted install helpers should be configured with repository and token access
that is no broader than the runner needs.

## Secrets

Do not put secrets in:

- MCP tool arguments
- prompt text
- checked-in environment files
- screenshots or log snippets selected for publication
- adapter examples

Use placeholders in documentation. Store real credentials in local environment
variables or an approved secret manager.

## Adapter Boundaries

The Rust MCP server owns Android lifecycle and artifact creation. Adapter
packages own model-facing item packaging.

This separation matters:

- local paths stay inside the host boundary
- screenshots become explicit image items
- XML/log/manifest artifacts become explicit file items
- direct OpenAI API use stays in the standalone adapter, not the MCP server

## Public Release Checklist

Before publishing a branch or release:

1. Run `git diff --check`.
2. Scan public docs and config for local usernames, hostnames, absolute paths,
   credentials, and product-specific examples that are not required by the
   committed public contract.
3. Confirm generated artifacts are either absent from the diff or intentionally
   reviewed.
4. Confirm any schema snapshot changes are intentional public contract changes.
5. Confirm the README, security notes, and adapter docs match the actual tool
   surface.
