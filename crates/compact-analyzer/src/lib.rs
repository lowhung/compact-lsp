// This file is part of compact-lsp.
// Copyright (C) 2025 Midnight Foundation
// SPDX-License-Identifier: Apache-2.0

//! Compact Analyzer - Analysis engines for the Compact LSP
//!
//! This crate provides:
//! - Diagnostics engine: wraps `compactc` compiler for error reporting
//! - Formatter engine: wraps `format-compact` for code formatting
//! - Parser engine: wraps tree-sitter for AST-based features

pub mod diagnostics;
pub mod formatter;
mod grammar;
pub mod parser;
pub mod toolchain;

pub use diagnostics::{CompilerCompatibility, CompilerInfo, DiagnosticEngine};
pub use formatter::FormatterEngine;
pub use parser::{
    CallArgument, CallHierarchyDocument, CallSite, CircuitCall, CircuitDefinition,
    CompletionSymbol, CompletionSymbolKind, DefinitionLocation, HoverInfo, ImportInfo,
    MemberAccessContext, ParameterInfo, ParserEngine, ReferenceLocation, SemanticToken,
    SemanticTokenModifier, SemanticTokenType, SignatureInfo, SourceIndex, SymbolLocation,
    SyntaxError,
};
pub use toolchain::{CompilerCommand, FormatterCommand, ToolSource};
