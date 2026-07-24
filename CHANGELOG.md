# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Compact CLI discovery with a selectable Compact 0.33 toolchain.
- Multi-root workspace indexing and live `.compact` file lifecycle updates.
- Checksummed, provenance-attested server archives and a VS Code extension.
- JSON-RPC regression coverage for workspace initialization and file events.
- Deterministic incoming and outgoing call hierarchy for local and prefixed
  imported circuits.
- Semantic selection ranges for identifiers, expressions, blocks, and
  declarations.
- Conservative linked editing for import prefixes and direct prefixed calls.

### Changed

- Pinned the maintained Compact tree-sitter grammar and added 0.33 fixtures.
- Made document synchronization, UTF-16 positions, and file URI handling
  protocol-correct.

### Fixed

- Compiler process cleanup, unique live-diagnostic sources, and paths containing
  spaces or non-ASCII characters.

## [0.1.0] - 2025-12-16

### Added
- Initial release
- Language Server Protocol (LSP) support for Compact smart contract language
- **Completion**: Auto-complete for circuits, structs, enums, and built-in types
- **Hover**: Documentation on hover for keywords, types, and symbols
- **Go to Definition**: Navigate to symbol definitions
- **Find References**: Find all usages of a symbol
- **Rename**: Rename symbols across files
- **Signature Help**: Function parameter hints while typing
- **Document Symbols**: Outline view of file structure
- **Folding Ranges**: Code folding for blocks and functions
- **Semantic Tokens**: Rich syntax highlighting
- **Diagnostics**: Real-time syntax error detection via tree-sitter
- **Formatting**: Code formatting via `format-compact`
- Cross-file symbol resolution via imports
