# Contributing

Thanks for helping improve `android-computer-use-mcp`.

## Development Principles

- Keep changes small, reviewable, and aligned with the existing tool contract.
- Prefer semantic Android interactions over raw coordinate control when a stable
  selector exists.
- Keep the Rust MCP server focused on Android lifecycle, artifact ownership, and
  truthful structured tool results.
- Keep model-specific item packaging in adapter packages rather than pushing it
  into the server.
- Treat schema snapshots as public contract evidence, not generated noise.

## Before Editing

Check the current branch and working tree:

```bash
git status --short --branch
```

If the branch tracks a remote branch, refresh remote refs before substantial
work:

```bash
git fetch origin --prune
```

Avoid bundling unrelated local changes into a commit.

## Validation

For behavior changes, run the smallest relevant checks. Common local commands
are:

```bash
cargo fmt --all
cargo test
cargo clippy --all-targets --all-features
```

For docs-only changes, compile-free validation is usually enough:

```bash
git diff --check
```

If a tool schema or resource catalog snapshot changes, make sure the change is
intentional and explain why in the PR or commit.

## Public Wording

This repository is a public surface. Keep examples neutral unless a specific
tool name or tested contract requires the app-specific wording.

Do not commit:

- local usernames or home-directory paths
- hostnames or runner identifiers
- tokens, API keys, or bearer values
- generated screenshots, XML, or logs that have not been reviewed
- comments that expose non-public operational details

Use placeholders such as `<checkout>`, `<android.package.name>`, and
`$HOME/Android/Sdk` in documentation.

## Pull Requests

In PR descriptions, include:

- what changed
- how it was validated
- whether any public MCP tool contract changed
- whether any generated artifacts or schema snapshots changed

If tests cannot be run, say why and provide the exact command a maintainer should
run.
