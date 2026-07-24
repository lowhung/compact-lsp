# Neovim Configuration for compact-lsp

## Setup

### 1. Add LSP config file

Create `~/.config/nvim/lua/core/lsp/configs/compact_lsp.lua`:

```lua
return {
    cmd = { "compact-lsp" },
    filetypes = { "compact" },
    root_markers = { ".git", "compact.toml", "package.json" },
    settings = {
        compact = {},
    },
}
```

### 2. Register filetype and enable LSP

Add to your LSP configuration (e.g., `lua/core/lsp.lua`):

```lua
-- Register .compact file extension
vim.filetype.add({
    extension = {
        compact = "compact",
    },
})

-- Register compact_lsp configuration
local compact_config = require("core.lsp.configs.compact_lsp")
compact_config.on_attach = on_attach
compact_config.capabilities = capabilities
vim.lsp.config("compact_lsp", compact_config)

-- Enable compact_lsp for .compact files
vim.api.nvim_create_autocmd("FileType", {
    pattern = { "compact" },
    callback = function()
        vim.lsp.enable("compact_lsp")
    end,
})
```

## Usage

1. Put the release binary in `PATH` and confirm it with
   `compact-lsp --version`.
2. Open any `.compact` file.
3. Check the connection with `:LspInfo`.
4. View logs with `:LspLog`.

The Compact compiler is separate. Install the `compact` CLI and toolchain 0.33,
or set `COMPACT_COMPILER` to a direct compiler binary before starting Neovim.

## Features

- **Diagnostics** - Compiler errors and warnings on save.
- **Completion** - Keywords, types, snippets, and cross-project symbols.
- **Hover** - Documentation for keywords, types, and symbols.
- **Go to Definition** - Navigate to definitions in the workspace.
- **References and Rename** - Inspect and update resolved workspace symbols.
- **Signature Help** - Show parameter hints while typing function calls.
- **Document Symbols** - Populate the outline with Compact declarations.
- **Folding** - Fold blocks and functions.
- **Formatting** - Format through the Compact CLI or `format-compact`.

## Keymaps

Standard LSP keymaps apply:

- `gd` - Go to definition.
- `K` - Hover documentation.
- `[d` / `]d` - Navigate diagnostics.
- `<leader>ld` - Show diagnostic float.
- `<C-Space>` - Trigger completion, if configured.
