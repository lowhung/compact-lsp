//! Type definitions for the parser module.

use lsp_types::Range;

/// Hover information result.
#[derive(Debug, Clone)]
pub struct HoverInfo {
    /// The content to display (markdown).
    pub content: String,
    /// The range of the hovered element.
    pub range: Option<Range>,
}

/// Definition location result.
#[derive(Debug, Clone)]
pub struct DefinitionLocation {
    /// The range where the definition is located.
    pub range: Range,
    /// The range of just the symbol name (for selection).
    pub selection_range: Range,
}

/// One direct circuit call made from a Compact circuit body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CircuitCall {
    /// Source-level call name, including an import prefix when one is present.
    pub name: String,
    /// UTF-16 range of the called name, excluding arguments.
    pub range: Range,
}

/// A circuit declaration and the direct calls contained in its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CircuitDefinition {
    /// Unprefixed declaration name.
    pub name: String,
    /// Range of the complete circuit declaration.
    pub range: Range,
    /// Range of the declaration name.
    pub selection_range: Range,
    /// Complete, non-member calls in source order.
    pub calls: Vec<CircuitCall>,
}

/// Parser output needed to build a workspace call hierarchy.
///
/// Circuits and imports come from the same tree so a hierarchy request never
/// resolves calls against import data from a different parse snapshot.
#[derive(Debug, Clone, Default)]
pub struct CallHierarchyDocument {
    /// Circuit declarations and their direct calls.
    pub circuits: Vec<CircuitDefinition>,
    /// File imports used to resolve prefixed call names.
    pub imports: Vec<ImportInfo>,
}

/// Parameter information for signature help.
#[derive(Debug, Clone)]
pub struct ParameterInfo {
    /// Parameter label (e.g., "a: Field").
    pub label: String,
}

/// Signature information result.
#[derive(Debug, Clone)]
pub struct SignatureInfo {
    /// The full signature label (e.g., "circuit add(a: Field, b: Field): Field").
    pub label: String,
    /// Documentation for the signature.
    pub documentation: Option<String>,
    /// Parameters with their labels.
    pub parameters: Vec<ParameterInfo>,
    /// The index of the active parameter (0-based).
    pub active_parameter: u32,
}

/// One argument in a syntactically complete Compact call expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallArgument {
    /// UTF-16 LSP position where the argument expression starts.
    pub position: lsp_types::Position,
    /// Source text for noise filtering, such as omitting `value:` before `value`.
    pub text: String,
}

/// A syntactically complete function or ledger-method call.
///
/// `receiver` is `None` for calls such as `hash(value)` and contains the
/// ledger receiver for calls such as `rounds.increment(value)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    /// Called circuit or method name.
    pub function_name: String,
    /// Ledger receiver for a member call.
    pub receiver: Option<String>,
    /// Arguments in source order.
    pub arguments: Vec<CallArgument>,
}

/// Symbol kind for completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionSymbolKind {
    Function,
    Struct,
    Enum,
    Variable,
    Module,
}

/// Location of a symbol in the source code.
#[derive(Debug, Clone)]
pub struct SymbolLocation {
    /// Start line (0-based).
    pub start_line: u32,
    /// Start character (0-based).
    pub start_char: u32,
    /// End line (0-based).
    pub end_line: u32,
    /// End character (0-based).
    pub end_char: u32,
}

/// A symbol for completion.
#[derive(Debug, Clone)]
pub struct CompletionSymbol {
    /// The symbol name.
    pub name: String,
    /// The kind of symbol.
    pub kind: CompletionSymbolKind,
    /// Detail text (e.g., "(a: Field, b: Field): Field").
    pub detail: Option<String>,
    /// Location of the symbol definition.
    pub location: Option<SymbolLocation>,
    /// Documentation for the symbol.
    pub documentation: Option<String>,
}

/// Information about an import statement.
#[derive(Debug, Clone)]
pub struct ImportInfo {
    /// The import path (e.g., "../utils/Utils" or "CompactStandardLibrary").
    pub path: String,
    /// True if this is a file import (quoted path), false if it's an identifier import.
    pub is_file: bool,
    /// The prefix for imported symbols (e.g., "Utils_").
    pub prefix: Option<String>,
}

/// Declarations and imports extracted from one parsed source tree.
///
/// Features that need both collections should use
/// [`ParserEngine::index_source`](crate::parser::ParserEngine::index_source)
/// instead of parsing the same document separately for completions and imports.
#[derive(Debug, Clone)]
pub struct SourceIndex {
    /// Declarations used by completion, navigation, and workspace-symbol features.
    pub symbols: Vec<CompletionSymbol>,
    /// Imports used to resolve prefixed symbols and maintain dependency edges.
    pub imports: Vec<ImportInfo>,
}

/// A syntax error detected by tree-sitter parsing.
#[derive(Debug, Clone)]
pub struct SyntaxError {
    /// The error message.
    pub message: String,
    /// The range where the error occurred.
    pub range: Range,
}

/// A semantic token for syntax highlighting.
#[derive(Debug, Clone)]
pub struct SemanticToken {
    /// The range of the token.
    pub range: Range,
    /// The type of the token.
    pub token_type: SemanticTokenType,
    /// Modifiers for the token.
    pub modifiers: Vec<SemanticTokenModifier>,
}

/// Semantic token types for syntax highlighting.
/// Order matters - these are indices into the LSP legend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SemanticTokenType {
    Function = 0,
    Type = 1,
    Struct = 2,
    Enum = 3,
    EnumMember = 4,
    Parameter = 5,
    Property = 6,
    Variable = 7,
    Namespace = 8,
    TypeParameter = 9,
}

/// Semantic token modifiers for syntax highlighting.
/// These are bit flags (1 << modifier_index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SemanticTokenModifier {
    Declaration = 0,
    Readonly = 1,
    DefaultLibrary = 2,
}

/// Context from a member access expression (e.g., `round.increment`).
#[derive(Debug, Clone)]
pub struct MemberAccessContext {
    /// The base identifier name (e.g., "round").
    pub base_name: String,
    /// The member identifier name (e.g., "increment").
    pub member_name: String,
    /// The range of the member identifier.
    pub member_range: Range,
}

/// A reference location for Find References.
#[derive(Debug, Clone)]
pub struct ReferenceLocation {
    /// The range of the reference.
    pub range: Range,
    /// True if this is the definition site, false if it's a usage.
    pub is_definition: bool,
}
