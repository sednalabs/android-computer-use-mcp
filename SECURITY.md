# Security Policy

## Supported Versions

This repository is pre-1.0. Security fixes are handled on the active development
branch unless a maintainer announces release branches.

## Reporting a Vulnerability

Do not file public issues for suspected vulnerabilities that include exploit
details, credentials, private hostnames, tokens, or sensitive screenshots.

Report privately to the repository maintainers through the project's preferred
private security contact. If no private contact is listed on the hosting
platform, open a minimal public issue asking for a private disclosure channel
without including sensitive details.

## Scope

Security-sensitive areas include:

- network binding and allowed-host validation
- command execution through configured Android SDK binaries
- APK install and launch helpers
- hosted interactive-session repository/token handling
- screenshot, UI XML, logcat, and manifest artifact handling
- adapter upload brokering and model-facing file/image item construction

## Operational Guidance

Keep deployments loopback-only. Use a dedicated artifact directory. Review
generated artifacts before publishing. Keep credentials out of prompts, tool
arguments, logs, and committed examples.

See [docs/SECURITY_MODEL.md](docs/SECURITY_MODEL.md) for the detailed local
threat model and public release checklist.
