# compact-lsp

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE-APACHE)
[![CI](https://github.com/lowhung/compact-lsp/actions/workflows/ci.yml/badge.svg)](https://github.com/lowhung/compact-lsp/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org)

This project extends the Midnight Network with additional developer tooling.

> **Beta:** This is a community-maintained project, not an officially supported
> Midnight Network component. Please report compatibility problems with the
> Compact compiler and language versions included in the bug report.

Language Server Protocol implementation for the [Compact](https://docs.midnight.network/develop/reference/compact/lang-ref) smart contract language (Midnight network).

## Features

| Feature | Description |
|---------|-------------|
| **Diagnostics** | Real-time syntax errors + compiler errors on save |
| **Semantic Tokens** | Rich syntax highlighting (functions, types, parameters, etc.) |
| **Completion** | Keywords, types, snippets, local and imported symbols |
| **Hover** | Documentation for keywords, types, and symbols |
| **Go to Definition** | Jump to symbol definitions (local and imported) |
| **Find References** | Find all usages of a symbol (local and cross-file) |
| **Rename** | Rename symbols across the workspace |
| **Signature Help** | Parameter hints while typing function calls |
| **Document Symbols** | Outline view (circuits, structs, enums, modules) |
| **Formatting** | Code formatting via the Compact CLI or `format-compact` |
| **Folding Ranges** | Code folding for blocks and functions |
| **Code Actions** | Safe quick fixes for Compact syntax diagnostics |
| **Cross-file Errors** | Errors propagate to dependent files on save |

### Cross-Project Support

Works with Compact's import system across every workspace folder:

```compact
import "./Utils" prefix Utils_;

Utils_add(5, 5);  // Completion, hover, go-to-def, find refs, rename, signature help all work
```

The server indexes workspace files without blocking editor requests and
registers a `**/*.compact` file watcher when the client supports it. Creating,
changing, or deleting an imported file updates completion and navigation
without restarting the language server.

## Requirements

- The `compact` CLI with a Compact 0.33 toolchain (recommended)
- A direct `compactc` and `format-compact` installation is also supported

## Installation

### VS Code

Download `compact-lsp-vscode-<version>.vsix` from the matching
[GitHub release](https://github.com/lowhung/compact-lsp/releases), then install
it:

```bash
code --install-extension compact-lsp-vscode-v0.2.0.vsix
```

The extension uses `compact-lsp` from `PATH` when available. Otherwise it
downloads the checksummed server archive for the current platform. See the
[VS Code setup and settings](editors/vscode/README.md).

### Server binary

Release archives are available for macOS (Apple Silicon and Intel), Linux
x86-64, and Windows x86-64. Verify an archive against `SHA256SUMS`, extract it,
and place `compact-lsp` (or `compact-lsp.exe`) in `PATH`.

Release artifacts also carry GitHub build-provenance attestations:

```bash
gh attestation verify compact-lsp-macos-arm64.tar.gz --repo lowhung/compact-lsp
```

### Build from source

A Rust toolchain is only required when building locally:

```bash
cargo build --workspace --release --locked
```

Binary: `target/release/compact-lsp`

Confirm which server an editor will launch with `compact-lsp --version`.

## Compact Toolchain

The LSP prefers the current `compact` CLI and falls back to direct compiler
binaries. Discovery order is:

1. `COMPACT_COMPILER` or `COMPACT_FORMATTER` for explicit direct binaries
2. `COMPACT_CLI`
3. `compact` in `PATH` or `~/.local/bin/compact`
4. `compactc`, `compactc.bin`, or `format-compact` in `PATH`
5. Modern `~/.compact/bin` and legacy `~/compactc` binary locations

Set `COMPACT_TOOLCHAIN_VERSION` to select a compiler installed by the Compact
CLI:

```bash
export COMPACT_TOOLCHAIN_VERSION=0.33.0
```

Project-specific compiler flags are accepted as a JSON string array. For
example, secp256k1 Compact programs require ZKIR v3:

```bash
export COMPACT_COMPILER_ARGS='["--feature-zkir-v3"]'
```

At startup, the server logs the detected compiler and language versions and
warns when the compiler is outside the primary Compact 0.33 compatibility
target.

## Related Projects

- [compact.vim](https://github.com/1NickPappas/compact.vim) - Vim/Neovim syntax highlighting
- [compact-tree-sitter](https://github.com/midnames/compact-tree-sitter) - Maintained Tree-sitter grammar fork

## Neovim Setup

### 1. Create LSP config

Create `~/.config/nvim/lua/lsp/compact.lua`:

```lua
return {
    cmd = { "compact-lsp" },
    filetypes = { "compact" },
    root_markers = { ".git", "compact.toml", "package.json" },
}
```

### 2. Register filetype and enable LSP

Add to your Neovim config:

```lua
-- Register .compact filetype
vim.filetype.add({
    extension = { compact = "compact" },
})

-- Load and register compact LSP
local compact_config = require("lsp.compact")
vim.lsp.config("compact_lsp", compact_config)

-- Auto-enable for .compact files
vim.api.nvim_create_autocmd("FileType", {
    pattern = "compact",
    callback = function()
        vim.lsp.enable("compact_lsp")
    end,
})
```

### 3. Verify

Open a `.compact` file and run:
```vim
:LspInfo
```

### Optional: Enable semantic highlighting

Add to your config to use LSP semantic tokens for highlighting:

```lua
vim.api.nvim_create_autocmd("LspAttach", {
    callback = function(args)
        local client = vim.lsp.get_client_by_id(args.data.client_id)
        if client and client.server_capabilities.semanticTokensProvider then
            vim.lsp.semantic_tokens.start(args.buf, args.data.client_id)
        end
    end,
})
```

## License

Most of the project is licensed under the [MIT License](LICENSE). The Compact
analyzer and selected server metadata components retain their
[Apache-2.0](LICENSE-APACHE) license and Midnight Foundation attribution; see
[NOTICE](NOTICE).
