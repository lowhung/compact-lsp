# Local testing

The repository has separate test tiers because a passing parser or protocol
test does not prove that a real Compact compiler or editor works. Start with the
hermetic smoke, then run the tier that matches what you changed or want to
evaluate.

## Fresh-clone smoke

Prerequisite: install Rust through [`rustup`](https://rustup.rs/).

```bash
git clone https://github.com/lowhung/compact-lsp.git
cd compact-lsp
cargo smoke
```

Cargo downloads the locked Rust dependencies on the first run. The smoke then:

1. Starts Cargo's exact `compact-lsp` integration-test binary over stdio.
2. Negotiates LSP capabilities and waits for the server-ready notification.
3. Opens the checked-in `test-fixtures/client-smoke` workspace.
4. Checks imported completion, workspace symbols, hover, definition, document
   symbols, and semantic tokens.
5. Opens the intentionally broken fixture and checks its syntax diagnostic and
   missing-semicolon quick fix.
6. Performs the LSP shutdown handshake and requires a clean process exit.

The command sets an explicit nonexistent compiler path. That makes it hermetic:
an installed Compact compiler cannot change the result. It proves parser and
representative protocol behavior, but it does not prove compiler diagnostics,
formatting, or editor integration.

## Full Rust validation

Run the same checks enforced for Rust changes:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
cargo test --workspace --all-targets --all-features --locked
cargo build --workspace --release --locked
```

The normal test command includes the fresh-clone smoke. It also exercises the
analyzer, workspace lifecycle, cancellation, JSON-RPC behavior, and performance
guards. These checks remain hermetic unless a test explicitly opts into an
external toolchain.

## Real Compact 0.33 compiler

Pass the absolute path to a Compact 0.33 compiler to the ignored compatibility
fixture:

```bash
COMPACT_LSP_TEST_COMPILER=/absolute/path/to/compactc \
  cargo test -p compact-analyzer --test toolchain_runner \
  validates_fixture_with_real_compact_0_33_compiler -- --ignored
```

This validates the compiler invocation and the checked-in Compact 0.33
contract. It does not start an editor. When testing the server through an
editor, set `COMPACT_TOOLCHAIN_VERSION=0.33.0`; the server discovers the
`compact` CLI automatically, or you can set `COMPACT_COMPILER` and
`COMPACT_FORMATTER` to direct binaries.

## Zed

Until the extension is published, use it as a development extension:

1. Build this checkout:

   ```bash
   cargo build -p compact-lsp --release --locked
   ```

2. Clone [`lowhung/compact-zed`](https://github.com/lowhung/compact-zed) and
   select that directory with `zed: install dev extension`.
3. Open `test-fixtures/client-smoke` in Zed.
4. Set the `lsp.compact.binary.path` workspace override to the absolute
   `target/release/compact-lsp` path.
5. Follow the [Zed smoke checklist](client-compatibility.md#zed-smoke).

This path uses local source only; it does not require or test a published
release.

## VS Code

Build the server and extension:

```bash
cargo build -p compact-lsp --release --locked
cd editors/vscode
npm ci
npm run lint
npm test
```

Start an isolated Extension Development Host from `editors/vscode`:

```bash
code \
  --user-data-dir /tmp/compact-lsp-vscode-user \
  --extensions-dir /tmp/compact-lsp-vscode-extensions \
  --extensionDevelopmentPath "$PWD" \
  ../../test-fixtures/client-smoke
```

Set `compact.server.path` to the absolute
`target/release/compact-lsp` path, then follow the
[VS Code smoke checklist](client-compatibility.md#vs-code-smoke). This tests
the local extension and server without automatic release download.

## Neovim

Neovim 0.11 or newer can run the checked-in headless client smoke:

```bash
cargo build -p compact-lsp --release --locked
COMPACT_LSP_SMOKE_SERVER="$PWD/target/release/compact-lsp" \
COMPACT_LSP_SMOKE_ROOT="$PWD/test-fixtures/client-smoke" \
NVIM_LOG_FILE=/tmp/compact-lsp-nvim.log \
XDG_STATE_HOME=/tmp/compact-lsp-nvim-state \
  nvim --headless -i NONE -u editors/nvim/smoke.lua
```

The fixture supplies hermetic compiler and formatter stubs for this client
check. Repeat the editor workflow with a real Compact 0.33 toolchain when
validating compiler diagnostics and formatting.

## Evidence boundaries

| Tier | Proves | Does not prove |
|---|---|---|
| `cargo smoke` | Real server startup, LSP framing, representative parser and language features, clean shutdown | Real compiler, formatter, editor, or release installation |
| Full Rust validation | Workspace-wide unit, integration, protocol, cancellation, and performance behavior | Real compiler or editor integration |
| Real compiler fixture | Compact 0.33 compiler discovery/invocation for the compatibility contract | Editor registration or client behavior |
| Local editor smoke | The selected client starts the local server and exposes its features | Published download, upgrade, or registry behavior |

Publication and managed-download checks are separate release gates. Do not use
a local smoke result as evidence that an unpublished registry or release path
works.
