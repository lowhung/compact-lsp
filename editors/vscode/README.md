# Compact Language Server for VS Code

This extension registers `.compact` files and starts `compact-lsp` for Compact
0.33 projects.

## Server installation

At activation, the extension uses the first available option:

1. `compact.server.path`
2. `compact-lsp` in `PATH` or `~/.local/bin`
3. A checksummed binary from the configured GitHub release

Automatic downloads support macOS on Apple Silicon and Intel, Linux x86-64,
and Windows x86-64. The `Compact: Install or Update Language Server` command
forces a fresh release download.

The Compact compiler remains a separate prerequisite. Install the `compact`
CLI and its 0.33 toolchain, or set `compact.compiler.path` to a direct
`compactc` binary.

## Settings

| Setting | Purpose |
|---------|---------|
| `compact.server.path` | Use a locally installed `compact-lsp` binary |
| `compact.server.autoDownload` | Allow automatic release installation |
| `compact.server.repository` | Select the GitHub release repository |
| `compact.server.version` | Install `latest` or a pinned tag such as `v0.2.0` |
| `compact.toolchain.version` | Select the Compact CLI toolchain; defaults to `0.33.0` |
| `compact.compiler.path` | Use a direct Compact compiler |
| `compact.formatter.path` | Use a direct Compact formatter |
| `compact.compiler.arguments` | Add project flags such as `--feature-zkir-v3` |

## Linked editing

Set `"editor.linkedEditing": true` to mirror edits between a Compact import
prefix declaration and the same prefix segment in direct calls. The server
returns no ranges for ambiguous or incomplete constructs. See the
[linked-editing contract and test example](../../docs/linked-editing.md).

## Semantic refactors

Select an exact, effect-free expression inside a Compact `return` or local
`const`, then use the lightbulb or **Quick Fix...** menu and choose
**Extract to local `extractedValue`**. Unsafe or incomplete selections do not
offer an edit. See the [refactor safety contract](../../docs/refactors.md).

## Release verification

The extension verifies the selected archive against the release
`SHA256SUMS` file before extraction. Release archives also carry GitHub
artifact provenance and can be verified independently:

```bash
gh attestation verify compact-lsp-macos-arm64.tar.gz --repo lowhung/compact-lsp
```

## Extension development

```bash
npm ci
npm run lint
npm test
npm run package
```
