# IDE client compatibility

The server follows the Language Server Protocol, but protocol tests alone do
not prove that an editor registers Compact files, starts the binary, exposes
features, and shuts it down correctly. This matrix uses one checked-in
[Compact 0.33 smoke workspace](../test-fixtures/client-smoke) in Zed, VS Code,
and Neovim.

## Supported clients

| Client | Supported version | Validation |
|---|---|---|
| Zed with `compact-zed` | Zed 1.12.0, extension 1.2.0 | Manual smoke before release |
| VS Code | 1.91.0 or newer | Manual smoke before release; extension unit tests in CI |
| Neovim | 0.11.0 or newer | Headless 0.11.5 smoke in CI and manual smoke before release |

These versions describe the current tested baseline, not a claim that older
clients cannot work. Record a new version in the release evidence whenever a
client is upgraded.

## Capability matrix

| Workflow | Zed | VS Code | Neovim 0.11 |
|---|---:|---:|---:|
| Start, restart, and shutdown | Yes | Yes | Yes |
| Completion and signature help | Yes | Yes | Yes |
| Hover, definition, references, and rename | Yes | Yes | Yes |
| Document and workspace symbols | Yes | Yes | Yes |
| Diagnostics and code actions | Yes | Yes | Yes |
| Formatting | Yes | Yes | Yes |
| Semantic tokens | Yes | Yes | Yes |
| Folding and document highlights | Yes | Yes | Yes |

Known client differences:

- Zed uses the separate
  [`lowhung/compact-zed`](https://github.com/lowhung/compact-zed) extension for
  language registration and managed server installation.
- VS Code exposes `Compact: Restart Language Server` and
  `Compact: Show Language Server Output` through the bundled extension.
- Neovim uses its built-in LSP client. The automated smoke requests semantic
  tokens directly; a user configuration decides how those tokens are rendered.
- Compiler diagnostics require a Compact compiler compatible with the project.
  Syntax diagnostics and the checked-in client smoke remain available without
  a real compiler.

## Shared smoke workspace

Build the exact server under test:

```bash
cargo build --workspace --release --locked
export COMPACT_LSP_SMOKE_SERVER="$PWD/target/release/compact-lsp"
export COMPACT_LSP_SMOKE_ROOT="$PWD/test-fixtures/client-smoke"
```

`Main.compact` contains local and prefixed imported calls, documentation,
ledger state, and semantic-token targets. `Broken.compact` contains one missing
semicolon for syntax diagnostics and the safe quick fix. The two executable
stubs make automated client tests hermetic; manual release testing should also
use the real Compact 0.33 toolchain.

Every manual client smoke must exercise:

1. Open `Main.compact` and confirm the Compact language and `compact-lsp`
   attachment.
2. Complete `Utils_scale`, hover `add`, and navigate from `Utils_scale` to
   `Utility.compact`.
3. Find references for `add`, preview a workspace rename, and cancel the edit.
4. Confirm document symbols, semantic highlighting, folding, and document
   highlights.
5. Format `Main.compact`.
6. Open `Broken.compact`, confirm the missing-semicolon syntax diagnostic, and
   inspect the `Insert missing ';'` quick fix.
7. Restart the language server and repeat completion.
8. Close the workspace and confirm the client log contains no crash, protocol
   error, or orphaned server process.

## Zed smoke

1. Build `compact-zed` and install it with `zed: install dev extension` from
   the command palette.
2. Open `test-fixtures/client-smoke` as a workspace.
3. Add a workspace `settings.json` override while testing a local server:

   ```json
   {
     "languages": {
       "Compact": {
         "language_servers": ["compact"],
         "semantic_tokens": "combined"
       }
     },
     "lsp": {
       "compact": {
         "binary": {
           "path": "/absolute/path/to/target/release/compact-lsp",
           "arguments": [],
           "env": {
             "COMPACT_TOOLCHAIN_VERSION": "0.33.0",
             "RUST_LOG": "compact_lsp=debug,compact_analyzer=debug"
           }
         }
       }
     }
   }
   ```

4. Run the shared checklist above.
5. Use `zed: open log` for actionable startup and protocol output. Confirm the
   log names the expected local binary.

Remove the binary override to test managed download and restart Zed. That
release-distribution check is required only after a public beta artifact exists.

## VS Code smoke

1. Install extension dependencies:

   ```bash
   cd editors/vscode
   npm ci
   npm run compile
   ```

2. Launch an isolated Extension Development Host:

   ```bash
   code \
     --user-data-dir /tmp/compact-lsp-vscode-user \
     --extensions-dir /tmp/compact-lsp-vscode-extensions \
     --extensionDevelopmentPath "$PWD" \
     "$COMPACT_LSP_SMOKE_ROOT"
   ```

3. Set `compact.server.path` to `COMPACT_LSP_SMOKE_SERVER`.
4. Run the shared checklist above.
5. Use `Compact: Show Language Server Output` for the server path, lifecycle,
   and protocol errors.

The automatic-download path needs a published release and is a separate release
gate from local-binary compatibility.

## Neovim smoke

Run the hermetic headless client check:

```bash
NVIM_LOG_FILE=/tmp/compact-lsp-nvim.log \
XDG_STATE_HOME=/tmp/compact-lsp-nvim-state \
  nvim --headless -i NONE -u editors/nvim/smoke.lua
```

The script starts the configured server, checks negotiated capabilities,
completion, hover, definition, symbols, semantic tokens, formatting, rename,
syntax diagnostics, restart, and clean shutdown. It deliberately disables
dynamic file-watcher registration because file lifecycle behavior is already
covered by the JSON-RPC protocol suite.

For a manual failure, use `:LspInfo`, `:checkhealth vim.lsp`, and `:LspLog`.

## Release evidence

Copy this table into the release issue or pull request and replace every value:

| Client | Client version | Extension version | Server commit | Toolchain | Result | Evidence |
|---|---|---|---|---|---|---|
| Zed | | | | Compact 0.33 | | Log or screenshot |
| VS Code | | | | Compact 0.33 | | Output log or screenshot |
| Neovim | | Built-in | | Compact 0.33 | | CI run and manual log |

A beta is not ready while any row is missing, failed, or tested against a
different server commit. Client-specific failures must include the client log,
server version, Compact toolchain version, operating system, and a minimal
reproduction based on the shared fixture.
