# Compact LSP development plan

## Compatibility contract

The first public release targets Compact toolchain 0.33 and Compact language
0.25. Toolchain 0.32 is best-effort compatible where it does not require a
separate implementation path. It is not a release gate.

Compatibility is measured against:

- The latest published Compact 0.33 compiler release.
- A checked-in parser fixture covering current modules, selective imports,
  contract references, cross-contract calls, and secp256k1 types.
- A small corpus of compiler-valid contracts from the matching Compact source
  tag.

The server should report its own version and the detected compiler version at
startup. A newer unsupported compiler should produce a warning, not silently
claim full compatibility.

## Release milestones

### 1. Parser and build foundation

- Pin a maintained Compact tree-sitter grammar revision.
- Add Compact 0.33 parser fixtures and a compiler-valid compatibility corpus.
- Vendor the generated parser from a pinned maintained grammar revision so
  builds and Cargo source packages do not depend on Git.
- Make `cargo fmt --all -- --check`, Clippy with warnings denied, tests, locked
  release builds, and package checks pass in CI.
- Preserve the mixed MIT/Apache-2.0 source licensing in package metadata and
  include both license texts and attribution in release archives.

Exit criteria:

- Valid compatibility fixtures produce no tree-sitter diagnostics.
- The workspace passes formatting, Clippy, tests, and release builds from a
  clean clone.
- Dependency revisions are reproducible.

### 2. Compact toolchain integration

- Discover the current `compact` CLI and its installed 0.33 toolchain before
  falling back to a directly configured `compactc.bin`.
- Support explicit compiler and formatter configuration without relying on
  shell-specific `which` behavior.
- Support project-level compiler arguments such as `--feature-zkir-v3` so
  diagnostics use the same language features as project builds.
- Parse compiler diagnostics for paths containing spaces and non-ASCII
  characters.
- Use unique, safely created temporary source files while preserving relative
  import resolution.
- Kill superseded compiler processes and reject stale diagnostic results.
- Generate standard-library completion and hover data from the matching
  compiler source instead of maintaining a hand-written registry.

Exit criteria:

- A new user with `compact` and toolchain 0.33 installed receives compiler
  diagnostics and formatting without custom environment variables.
- Saving or rapidly editing two files cannot cross-contaminate diagnostics.
- The standard-library registry is versioned and checked against its source.

### 3. LSP protocol correctness

- Convert file URIs with a standards-compliant URI library, including encoded
  spaces, Windows drive paths, and UNC paths.
- Convert all LSP positions as UTF-16 code units and all tree-sitter positions
  as UTF-8 bytes.
- Either implement incremental document changes correctly or advertise full
  document synchronization until that implementation is proven.
- Enforce monotonically increasing document versions.
- Return valid full-document formatting ranges.
- Add JSON-RPC integration tests for initialize, open, change, save, close,
  diagnostics, cancellation, shutdown, and malformed input.

Exit criteria:

- The protocol suite passes with ASCII, emoji, combining characters, encoded
  paths, and out-of-order changes.
- No request can panic the server or leave a compiler child running after
  cancellation.

### 4. Workspace semantics

- Index every workspace folder without blocking the async runtime.
- Follow create, rename, delete, and content changes through file watchers.
- Bound symlink traversal and cache growth.
- Model Compact 0.33 modules, selective imports, prefixes, contract types, and
  cross-contract member calls.
- Make rename and references conservative when resolution is ambiguous.

Exit criteria:

- Multi-root workspaces and file lifecycle changes update navigation without a
  restart.
- Import, definition, reference, completion, and rename tests use real
  multi-file Compact 0.33 projects.

### 5. IDE distribution

- Publish signed server archives with checksums for supported macOS and Linux
  targets. Build Windows server binaries, but only promise compiler-backed
  diagnostics where an official Compact compiler is available.
- Add a minimal VS Code extension that installs or locates the server, registers
  `.compact`, exposes settings, and shows actionable startup failures.
- Keep a documented generic LSP setup for Neovim and other editors.
- Add an end-to-end VS Code smoke test that opens a fixture and observes
  initialization, completion, navigation, diagnostics, formatting, and clean
  shutdown.

Exit criteria:

- A user can install the VS Code extension and use Compact 0.33 without building
  Rust or manually writing client configuration.
- Release artifacts are reproducible, checksummed, and exercised before
  publishing.

### 6. Public beta and stable release

- Document supported features, limitations, configuration, logs, diagnostics,
  security reporting, and upgrade behavior.
- Test upgrade and rollback between two server releases.
- Triage all crash, data-loss, protocol-corruption, and false-diagnostic bugs as
  release blockers.
- Establish upstream ownership for the LSP and maintained grammar, or document
  the fork governance and release process explicitly.

Exit criteria:

- Beta feedback contains no open release-blocking correctness issues.
- The repository has a supported release channel, issue templates, changelog,
  contribution path, and named maintainers.

## Proposed pull request sequence

1. `[fix] Target Compact 0.33 parser and restore quality gates`
2. `[feat] Integrate the Compact toolchain and versioned standard library`
3. `[fix] Make document synchronization and positions LSP-correct`
4. `[feat] Add resilient multi-file Compact 0.33 workspace semantics`
5. `[feat] Ship VS Code client and signed server artifacts`
6. `[chore] Prepare the first public beta`

Each pull request should leave the default branch releasable and include its
own regression tests. The first four are server correctness work; editor
distribution starts only after their exit criteria are met.
