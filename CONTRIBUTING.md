# Contributing to compact-lsp

Thank you for contributing to compact-lsp. This document covers the checks and
environment expected by the project.

## Getting Started

1. Fork the repository.
2. Clone your fork: `git clone https://github.com/YOUR_USERNAME/compact-lsp.git`.
3. Create a branch: `git switch -c feature/your-feature-name`.

## Development Setup

### Prerequisites

- Rust 1.70+ (install via [rustup](https://rustup.rs/)).
- The `compact` CLI with toolchain 0.33 for compiler-backed integration tests.
- Node.js 20 when changing the VS Code extension.

### Building

```bash
cargo build --workspace --locked
```

### Running Tests

```bash
cargo test --workspace --all-targets --all-features --locked
```

The real compiler fixture is opt-in so routine tests remain hermetic:

```bash
COMPACT_LSP_TEST_COMPILER=/path/to/compactc \
  cargo test -p compact-analyzer --test toolchain_runner \
  validates_fixture_with_real_compact_0_33_compiler -- --ignored
```

For extension changes:

```bash
cd editors/vscode
npm ci
npm run lint
npm test
npm run package
```

### Code Style

This project uses `rustfmt` and `clippy` for code formatting and linting:

```bash
# Format code
cargo fmt --all

# Check formatting (CI will fail if not formatted)
cargo fmt --all -- --check

# Run linter
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# Check public documentation and links
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
```

## Making Changes

### Code Guidelines

- Follow Rust idioms and best practices.
- Keep functions focused and small.
- Document public functions and types. Document internal helpers when their
  invariants, failure behavior, or performance role are not obvious from the
  implementation.
- Add tests for new functionality.
- Update documentation for user-visible behavior and configuration.
- Add Rustdoc to public APIs and non-obvious private helpers. Explain the
  function's purpose, important invariants, and why it returns no result or an
  error instead of guessing.
- Document protocol boundaries such as UTF-16 positions, half-open ranges,
  version ordering, cancellation, ambiguity handling, and partial-syntax
  fallbacks where they affect correctness.
- Preserve the vendored grammar provenance and hashes when updating it.

### Commit Messages

- Use clear, descriptive commit messages.
- Start with a verb (Add, Fix, Update, Remove, etc.).
- Keep the first line under 72 characters.

Good examples:

- `Add hover support for ledger declarations`
- `Fix goto definition for imported symbols`
- `Update README with Neovim setup instructions`

## Submitting Changes

1. Run the formatting, Clippy, and test commands above.
2. Run the VS Code checks when `editors/vscode` changes.
3. Push to your fork.
4. Open a pull request with the Compact versions used for validation.

### Pull Request Guidelines

- Provide a clear description of the changes.
- Reference any related issues.
- Include screenshots for visible editor changes.
- Add regression tests for fixes and new features.

## Reporting Bugs

When reporting bugs, please include:

- `compact-lsp --version`.
- Compact CLI, compiler, and language versions.
- Operating system and architecture.
- Editor or IDE and version.
- Steps to reproduce and a minimal `.compact` source.
- Relevant language-server output with secrets and local identifiers removed.

## Requesting Features

Feature requests are welcome! Please:

- Check whether the feature already exists or is planned.
- Describe the editor workflow or language construct involved.
- Explain the expected behavior.

## Code of Conduct

Follow the project [Code of Conduct](CODE_OF_CONDUCT.md).

## Questions?

Open a GitHub discussion or issue for questions that are not security reports.
