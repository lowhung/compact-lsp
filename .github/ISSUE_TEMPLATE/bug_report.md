---
name: Bug Report
about: Report a bug to help us improve
title: '[Bug] '
labels: bug
assignees: ''
---

## Description

A clear and concise description of what the bug is.

## Steps to Reproduce

1. Open file '...'
2. Type '...'
3. See error

## Expected Behavior

What you expected to happen.

## Actual Behavior

What actually happened.

## Environment

- OS: [e.g., macOS 14.0, Ubuntu 22.04, Windows 11]
- Architecture: [e.g., arm64, x86-64]
- Editor: [e.g., Neovim 0.11, VS Code 1.91]
- compact-lsp version: [output of `compact-lsp --version`]
- Extension version, if applicable: [e.g., 0.2.0]
- Compact CLI version: [output of `compact --version`]
- Compact toolchain selection: [e.g., 0.33.0]
- Compiler version: [output of `compact compile +0.33.0 --version`]
- Language version: [output of `compact compile +0.33.0 --language-version`]

## Minimal Compact Source

```compact
// Paste the smallest source that reproduces the problem.
```

## Logs

In VS Code, run `Compact: Show Language Server Output`. For other clients,
start the server with `RUST_LOG=debug`. Remove secrets and sensitive paths.

```
Paste logs here
```

## Additional Context

Add any other context about the problem here.
