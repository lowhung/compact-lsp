//! The main Language Server implementation.
//!
//! # LSP Lifecycle
//!
//! 1. Editor starts our binary and sends `initialize` request
//! 2. We respond with our capabilities (what features we support)
//! 3. Editor sends `initialized` notification (handshake complete)
//! 4. Normal operation: file events, requests flow both directions
//! 5. Editor sends `shutdown` request, we respond, then `exit` notification

mod builtins;
mod imports;
mod state;
mod stdlib;
mod utils;
mod validation;
mod workspace;

pub use state::Document;

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use compact_analyzer::{
    CompilerCompatibility, CompletionSymbol, DiagnosticEngine, FormatterEngine, ImportInfo,
    ParserEngine,
};
use dashmap::DashMap;
use lsp_types::*;
use ropey::Rope;
use tokio::sync::Mutex as AsyncMutex;
use tower_lsp::jsonrpc::Result;
use tower_lsp::{Client, LanguageServer};

struct IndexedWorkspaceFile {
    uri: String,
    content: String,
    symbols: Vec<CompletionSymbol>,
    imports: Vec<ImportInfo>,
}

/// The Compact Language Server.
///
/// This struct holds all the state needed by the server:
/// - `client`: Used to send notifications TO the editor (e.g., diagnostics)
/// - `documents`: Map of open files (Uri -> Document)
/// - `diagnostic_engine`: Wraps the compactc compiler
/// - `formatter_engine`: Wraps the format-compact binary
pub struct CompactLanguageServer {
    /// The LSP client - used to send messages TO the editor.
    client: Client,

    /// Open documents, keyed by their URI.
    documents: Arc<DashMap<String, Document>>,

    /// The diagnostic engine that wraps compactc.
    diagnostic_engine: Arc<DiagnosticEngine>,

    /// The formatter engine that wraps format-compact.
    formatter_engine: Arc<FormatterEngine>,

    /// The parser engine for tree-sitter based features.
    parser_engine: Arc<Mutex<ParserEngine>>,

    /// Workspace root URIs (captured from initialize params and folder changes).
    workspace_roots: Arc<Mutex<Vec<String>>>,

    /// Serializes full workspace scans while allowing the async runtime to keep serving.
    workspace_scan: Arc<AsyncMutex<()>>,

    /// Changes whenever a watched file or workspace folder invalidates a scan snapshot.
    workspace_epoch: Arc<AtomicU64>,

    /// Whether the client accepts dynamic watched-file registration.
    supports_watched_files: Arc<AtomicBool>,

    /// Symbol cache for cross-file completion.
    symbol_cache: Arc<DashMap<String, Vec<CompletionSymbol>>>,

    /// Source cache for cross-file hover and definition.
    source_cache: Arc<DashMap<String, String>>,

    /// Pending semantic diagnostics tasks.
    pending_diagnostics: Arc<DashMap<String, tokio::task::JoinHandle<()>>>,

    /// Reverse dependency map for cross-file error propagation.
    reverse_dependencies: Arc<DashMap<String, Vec<String>>>,
}

impl CompactLanguageServer {
    fn quick_fix_for_missing_token(
        uri: &Uri,
        diagnostic: &Diagnostic,
    ) -> Option<CodeActionOrCommand> {
        if diagnostic.source.as_deref() != Some("compact-syntax") {
            return None;
        }

        let token = diagnostic
            .message
            .strip_prefix("Syntax error: missing ")
            .and_then(Self::safe_missing_token)?;

        let mut changes = std::collections::HashMap::new();
        changes.insert(
            uri.clone(),
            vec![TextEdit {
                range: Range {
                    start: diagnostic.range.start,
                    end: diagnostic.range.start,
                },
                new_text: token.to_string(),
            }],
        );

        Some(
            CodeAction {
                title: format!("Insert missing `{token}`"),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diagnostic.clone()]),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    document_changes: None,
                    change_annotations: None,
                }),
                is_preferred: Some(true),
                ..Default::default()
            }
            .into(),
        )
    }

    fn safe_missing_token(kind: &str) -> Option<&'static str> {
        match kind {
            ";" => Some(";"),
            "," => Some(","),
            ":" => Some(":"),
            "(" => Some("("),
            ")" => Some(")"),
            "[" => Some("["),
            "]" => Some("]"),
            "{" => Some("{"),
            "}" => Some("}"),
            "<" => Some("<"),
            ">" => Some(">"),
            _ => None,
        }
    }

    fn quick_fixes_requested(params: &CodeActionParams) -> bool {
        match params.context.only.as_ref() {
            None => true,
            Some(kinds) => kinds
                .iter()
                .any(|kind| kind.as_str() == CodeActionKind::QUICKFIX.as_str()),
        }
    }

    /// Create a new language server instance.
    pub fn new(client: Client) -> Self {
        let diagnostic_engine = DiagnosticEngine::new();
        let formatter_engine = FormatterEngine::new();

        if diagnostic_engine.is_available() {
            tracing::info!("Compact compiler found");
        } else {
            tracing::warn!("Compact compiler not found - diagnostics will be unavailable");
        }

        if formatter_engine.is_available() {
            tracing::info!("Compact formatter found");
        } else {
            tracing::warn!("Compact formatter not found - formatting will be unavailable");
        }

        let parser_engine = ParserEngine::new();
        tracing::info!("Tree-sitter parser initialized");

        Self {
            client,
            documents: Arc::new(DashMap::new()),
            diagnostic_engine: Arc::new(diagnostic_engine),
            formatter_engine: Arc::new(formatter_engine),
            parser_engine: Arc::new(Mutex::new(parser_engine)),
            workspace_roots: Arc::new(Mutex::new(Vec::new())),
            workspace_scan: Arc::new(AsyncMutex::new(())),
            workspace_epoch: Arc::new(AtomicU64::new(0)),
            supports_watched_files: Arc::new(AtomicBool::new(false)),
            symbol_cache: Arc::new(DashMap::new()),
            source_cache: Arc::new(DashMap::new()),
            pending_diagnostics: Arc::new(DashMap::new()),
            reverse_dependencies: Arc::new(DashMap::new()),
        }
    }

    /// Publish diagnostics for a document.
    async fn publish_diagnostics(&self, uri: Uri) {
        let content = match self.documents.get(&uri.to_string()) {
            Some(doc) => doc.content.to_string(),
            None => {
                let Some(path) = imports::file_uri_to_path(&uri.to_string()) else {
                    return;
                };
                match tokio::fs::read_to_string(path).await {
                    Ok(content) => content,
                    Err(_) => return,
                }
            }
        };

        let syntax_diagnostics: Vec<Diagnostic> = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser
                .get_syntax_errors(&content)
                .into_iter()
                .map(|e| Diagnostic {
                    range: e.range,
                    severity: Some(DiagnosticSeverity::ERROR),
                    source: Some("compact-syntax".to_string()),
                    message: e.message,
                    ..Default::default()
                })
                .collect()
        };

        let compiler_diagnostics = self
            .diagnostic_engine
            .diagnose(&uri.to_string(), &content)
            .await;

        let mut all_diagnostics = syntax_diagnostics;
        all_diagnostics.extend(compiler_diagnostics);

        self.client
            .publish_diagnostics(uri, all_diagnostics, None)
            .await;
    }

    /// Publish syntax diagnostics for a document (instant, on every keystroke).
    async fn publish_syntax_diagnostics(&self, uri: Uri) {
        let content = match self.documents.get(&uri.to_string()) {
            Some(doc) => doc.content.to_string(),
            None => return,
        };

        let syntax_errors = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser.get_syntax_errors(&content)
        };

        let diagnostics: Vec<Diagnostic> = syntax_errors
            .into_iter()
            .map(|e| Diagnostic {
                range: e.range,
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("compact-syntax".to_string()),
                message: e.message,
                ..Default::default()
            })
            .collect();

        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }

    /// Schedule semantic diagnostics with debounce.
    async fn schedule_semantic_diagnostics(&self, uri: Uri, content: String) {
        let uri_string = uri.to_string();

        if let Some((_, handle)) = self.pending_diagnostics.remove(&uri_string) {
            handle.abort();
        }

        let client = self.client.clone();
        let diagnostic_engine = self.diagnostic_engine.clone();
        let parser_engine = self.parser_engine.clone();
        let pending = self.pending_diagnostics.clone();
        let uri_clone = uri_string.clone();

        let handle = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            let syntax_diagnostics: Vec<Diagnostic> = {
                let mut parser = parser_engine.lock().unwrap();
                parser
                    .get_syntax_errors(&content)
                    .into_iter()
                    .map(|e| Diagnostic {
                        range: e.range,
                        severity: Some(DiagnosticSeverity::ERROR),
                        source: Some("compact-syntax".to_string()),
                        message: e.message,
                        ..Default::default()
                    })
                    .collect()
            };

            let compiler_diagnostics = diagnostic_engine
                .diagnose_content(&uri_clone, &content)
                .await;

            let mut all_diagnostics = syntax_diagnostics;
            all_diagnostics.extend(compiler_diagnostics);

            client.publish_diagnostics(uri, all_diagnostics, None).await;
            pending.remove(&uri_clone);
        });

        self.pending_diagnostics.insert(uri_string, handle);
    }

    /// Scan workspace for all .compact files and cache their symbols.
    async fn scan_workspace(&self) {
        let _scan_guard = self.workspace_scan.lock().await;

        loop {
            let roots = self.workspace_roots.lock().unwrap().clone();
            if roots.is_empty() {
                tracing::warn!("No workspace roots set, skipping workspace scan");
                return;
            }

            let epoch = self.workspace_epoch.load(Ordering::Acquire);
            let indexed_files =
                match tokio::task::spawn_blocking(move || Self::index_workspace_roots(roots)).await
                {
                    Ok(files) => files,
                    Err(error) => {
                        tracing::error!("Workspace indexing task failed: {}", error);
                        return;
                    }
                };

            if epoch != self.workspace_epoch.load(Ordering::Acquire) {
                tracing::debug!("Workspace changed during indexing; restarting scan");
                continue;
            }

            self.replace_workspace_index(indexed_files);
            break;
        }
    }

    fn index_workspace_roots(roots: Vec<String>) -> Vec<IndexedWorkspaceFile> {
        let mut parser = ParserEngine::new();
        let mut indexed = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for root in roots {
            let Some(root_path) = imports::file_uri_to_path(&root) else {
                tracing::warn!("Workspace root is not a file URI: {}", root);
                continue;
            };

            tracing::info!(
                "Scanning workspace for .compact files: {}",
                root_path.display()
            );

            let entries = match workspace::find_compact_files(&root_path) {
                Ok(entries) => entries,
                Err(error) => {
                    tracing::warn!(
                        "Could not scan workspace root {}: {}",
                        root_path.display(),
                        error
                    );
                    continue;
                }
            };

            for file_path in entries {
                let canonical_path = file_path.canonicalize().unwrap_or(file_path);
                let Some(uri) = imports::path_to_file_uri(&canonical_path) else {
                    tracing::warn!(
                        "Could not convert workspace path to a file URI: {}",
                        canonical_path.display()
                    );
                    continue;
                };
                if !seen.insert(uri.clone()) {
                    continue;
                }

                let content = match std::fs::read_to_string(&canonical_path) {
                    Ok(content) => content,
                    Err(error) => {
                        tracing::warn!("Failed to read {}: {}", canonical_path.display(), error);
                        continue;
                    }
                };

                let symbols = parser.get_completion_symbols(&content);
                let file_imports = parser.get_imports(&content);
                indexed.push(IndexedWorkspaceFile {
                    uri,
                    content,
                    symbols,
                    imports: file_imports,
                });
            }
        }

        indexed
    }

    fn replace_workspace_index(&self, indexed_files: Vec<IndexedWorkspaceFile>) {
        self.source_cache.clear();
        self.symbol_cache.clear();
        self.reverse_dependencies.clear();

        let files_found = indexed_files.len();
        let symbols_found = indexed_files
            .iter()
            .map(|file| file.symbols.len())
            .sum::<usize>();

        for file in &indexed_files {
            self.source_cache
                .insert(file.uri.clone(), file.content.clone());
            if !file.symbols.is_empty() {
                self.symbol_cache
                    .insert(file.uri.clone(), file.symbols.clone());
            }
        }

        for file in indexed_files {
            self.add_reverse_dependencies(&file.uri, &file.imports);
        }

        let open_documents: Vec<_> = self
            .documents
            .iter()
            .map(|entry| (entry.key().clone(), entry.content.to_string()))
            .collect();
        for (uri, content) in open_documents {
            self.update_symbol_cache(&uri, &content);
            self.update_reverse_dependencies(&uri, &content);
        }

        let dependency_count = self
            .reverse_dependencies
            .iter()
            .map(|entry| entry.value().len())
            .sum::<usize>();
        tracing::info!(
            "Workspace scan complete: {} files, {} symbols, {} dependency edges",
            files_found,
            symbols_found,
            dependency_count
        );
    }

    fn normalized_file_uri(uri: &str) -> Option<String> {
        let path = imports::file_uri_to_path(uri)?;
        let normalized = path.canonicalize().ok().or_else(|| {
            let parent = path.parent()?.canonicalize().ok()?;
            Some(parent.join(path.file_name()?))
        });
        let normalized = normalized.or_else(|| imports::normalize_path(&path))?;
        imports::path_to_file_uri(&normalized)
    }

    fn cache_uri(uri: &str) -> String {
        Self::normalized_file_uri(uri).unwrap_or_else(|| uri.to_string())
    }

    fn workspace_contains_uri(&self, uri: &str) -> bool {
        let Some(path) =
            Self::normalized_file_uri(uri).and_then(|uri| imports::file_uri_to_path(&uri))
        else {
            return false;
        };
        if path.extension().and_then(|extension| extension.to_str()) != Some("compact") {
            return false;
        }

        self.workspace_roots.lock().unwrap().iter().any(|root| {
            imports::file_uri_to_path(root)
                .and_then(|root| {
                    root.canonicalize()
                        .ok()
                        .or_else(|| imports::normalize_path(&root))
                })
                .map(|root| path.starts_with(root))
                .unwrap_or(false)
        })
    }

    async fn refresh_workspace_file(&self, uri: &str) -> Option<String> {
        if !self.workspace_contains_uri(uri) {
            return None;
        }
        let cache_uri = Self::normalized_file_uri(uri)?;

        let content = if let Some(document) = self.documents.get(uri) {
            document.content.to_string()
        } else {
            let path = imports::file_uri_to_path(uri)?;
            match tokio::fs::read_to_string(path).await {
                Ok(content) => content,
                Err(error) => {
                    tracing::debug!("Could not refresh workspace file {}: {}", uri, error);
                    return None;
                }
            }
        };

        self.update_symbol_cache(&cache_uri, &content);
        self.update_reverse_dependencies(&cache_uri, &content);
        Some(cache_uri)
    }

    fn remove_workspace_file(&self, uri: &str) -> Vec<String> {
        let uri = Self::cache_uri(uri);
        self.source_cache.remove(&uri);
        self.symbol_cache.remove(&uri);
        self.remove_reverse_dependencies(&uri);
        self.reverse_dependencies
            .remove(&uri)
            .map(|(_, dependents)| dependents)
            .unwrap_or_default()
    }

    /// Update the symbol and source cache for a specific file.
    fn update_symbol_cache(&self, uri: &str, content: &str) {
        let uri = Self::cache_uri(uri);
        let symbols = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser.get_completion_symbols(content)
        };

        self.source_cache.insert(uri.clone(), content.to_string());

        if symbols.is_empty() {
            self.symbol_cache.remove(&uri);
        } else {
            self.symbol_cache.insert(uri, symbols);
        }
    }

    /// Update reverse dependencies for a file based on its imports.
    fn update_reverse_dependencies(&self, uri: &str, content: &str) {
        let uri = Self::cache_uri(uri);
        self.remove_reverse_dependencies(&uri);

        let file_imports = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser.get_imports(content)
        };

        self.add_reverse_dependencies(&uri, &file_imports);
    }

    fn add_reverse_dependencies(&self, uri: &str, file_imports: &[ImportInfo]) {
        for import in file_imports {
            if import.is_file {
                if let Some(imported_uri) = imports::resolve_import_path(uri, &import.path) {
                    let mut dependents = self.reverse_dependencies.entry(imported_uri).or_default();
                    if !dependents.iter().any(|dependent| dependent == uri) {
                        dependents.push(uri.to_string());
                    }
                }
            }
        }
    }

    /// Remove a file from all reverse dependency lists.
    fn remove_reverse_dependencies(&self, uri: &str) {
        let uri = Self::cache_uri(uri);
        for mut entry in self.reverse_dependencies.iter_mut() {
            entry.value_mut().retain(|dependent| dependent != &uri);
        }
        self.reverse_dependencies.retain(|_, deps| !deps.is_empty());
    }

    /// Get all files that depend on (import) the given file.
    fn get_dependents(&self, uri: &str) -> Vec<String> {
        let uri = Self::cache_uri(uri);
        self.reverse_dependencies
            .get(&uri)
            .map(|deps| deps.value().clone())
            .unwrap_or_default()
    }

    /// Find an imported symbol by name (with prefix handling).
    fn find_imported_symbol(
        &self,
        current_uri: &str,
        name: &str,
    ) -> Option<(String, CompletionSymbol)> {
        let file_imports = {
            let content = self.documents.get(current_uri)?.content.to_string();
            let mut parser = self.parser_engine.lock().unwrap();
            parser.get_imports(&content)
        };

        for import in file_imports {
            if !import.is_file {
                continue;
            }

            let prefix = match &import.prefix {
                Some(p) => p,
                None => continue,
            };

            if !name.starts_with(prefix) {
                continue;
            }

            let symbol_name = &name[prefix.len()..];
            if symbol_name.is_empty() {
                continue;
            }

            let resolved_uri = match imports::resolve_import_path(current_uri, &import.path) {
                Some(uri) => uri,
                None => continue,
            };

            if let Some(symbols) = self.symbol_cache.get(&resolved_uri) {
                for symbol in symbols.iter() {
                    if symbol.name == symbol_name {
                        return Some((resolved_uri, symbol.clone()));
                    }
                }
            }
        }

        None
    }

    /// Build a SignatureHelp response from SignatureInfo.
    fn build_signature_help_response(
        &self,
        info: compact_analyzer::SignatureInfo,
    ) -> SignatureHelp {
        let parameters: Vec<ParameterInformation> = info
            .parameters
            .iter()
            .map(|p| ParameterInformation {
                label: ParameterLabel::Simple(p.label.clone()),
                documentation: None,
            })
            .collect();

        let signature = SignatureInformation {
            label: info.label,
            documentation: info.documentation.map(|d| {
                Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: d,
                })
            }),
            parameters: Some(parameters),
            active_parameter: Some(info.active_parameter),
        };

        SignatureHelp {
            signatures: vec![signature],
            active_signature: Some(0),
            active_parameter: Some(info.active_parameter),
        }
    }

    /// Get the symbol names to search for in a given file (handles import prefixes).
    fn get_search_names_for_file(
        &self,
        searching_file: &str,
        defining_file: &str,
        symbol_name: &str,
    ) -> Vec<String> {
        let defining_file = Self::cache_uri(defining_file);
        let mut names = Vec::new();

        let content = match self.source_cache.get(searching_file) {
            Some(c) => c.clone(),
            None => return names,
        };

        let file_imports = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser.get_imports(&content)
        };

        for import in file_imports {
            if !import.is_file {
                continue;
            }
            if let Some(resolved) = imports::resolve_import_path(searching_file, &import.path) {
                if resolved == defining_file {
                    let prefixed_name = match &import.prefix {
                        Some(prefix) => format!("{}{}", prefix, symbol_name),
                        None => symbol_name.to_string(),
                    };
                    names.push(prefixed_name);
                }
            }
        }

        if !names.contains(&symbol_name.to_string()) {
            names.push(symbol_name.to_string());
        }

        names
    }

    async fn register_workspace_file_watcher(&self) {
        if !self.supports_watched_files.load(Ordering::Acquire) {
            tracing::debug!("Client does not support dynamic watched-file registration");
            return;
        }

        let options = DidChangeWatchedFilesRegistrationOptions {
            watchers: vec![FileSystemWatcher {
                glob_pattern: GlobPattern::String("**/*.compact".to_string()),
                kind: Some(WatchKind::Create | WatchKind::Change | WatchKind::Delete),
            }],
        };
        let register_options = match serde_json::to_value(options) {
            Ok(options) => options,
            Err(error) => {
                tracing::warn!("Could not serialize watched-file registration: {}", error);
                return;
            }
        };

        if let Err(error) = self
            .client
            .register_capability(vec![Registration {
                id: "compact-lsp-watch-compact-files".to_string(),
                method: "workspace/didChangeWatchedFiles".to_string(),
                register_options: Some(register_options),
            }])
            .await
        {
            tracing::warn!(
                "Could not register Compact workspace file watcher: {}",
                error
            );
        }
    }

    fn apply_workspace_folder_change(roots: &mut Vec<String>, event: WorkspaceFoldersChangeEvent) {
        for removed in event.removed {
            let removed = removed.uri.to_string();
            roots.retain(|root| root != &removed);
        }
        roots.extend(event.added.into_iter().map(|folder| folder.uri.to_string()));
        roots.sort();
        roots.dedup();
    }
}

/// Implementation of the Language Server Protocol.
impl LanguageServer for CompactLanguageServer {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        tracing::info!("Received initialize request");

        let supports_watched_files = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.did_change_watched_files)
            .and_then(|capability| capability.dynamic_registration)
            .unwrap_or(false);
        self.supports_watched_files
            .store(supports_watched_files, Ordering::Release);

        let mut workspace_roots: Vec<String> = params
            .workspace_folders
            .as_ref()
            .map(|folders| {
                folders
                    .iter()
                    .map(|folder| folder.uri.to_string())
                    .collect()
            })
            .unwrap_or_default();
        if workspace_roots.is_empty() {
            #[allow(deprecated)]
            if let Some(root_uri) = params.root_uri.as_ref() {
                workspace_roots.push(root_uri.to_string());
            }
        }
        workspace_roots.sort();
        workspace_roots.dedup();

        if workspace_roots.is_empty() {
            tracing::warn!("No workspace root provided by client");
        } else {
            for root in &workspace_roots {
                tracing::info!("Workspace root: {}", root);
            }
        }
        *self.workspace_roots.lock().unwrap() = workspace_roots;

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                position_encoding: Some(PositionEncodingKind::UTF16),
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                            include_text: Some(false),
                        })),
                        ..Default::default()
                    },
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![
                        ".".to_string(),
                        ":".to_string(),
                        "<".to_string(),
                    ]),
                    resolve_provider: Some(false),
                    ..Default::default()
                }),
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
                        resolve_provider: Some(false),
                        work_done_progress_options: Default::default(),
                    },
                )),
                document_formatting_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: Default::default(),
                })),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    retrigger_characters: None,
                    work_done_progress_options: Default::default(),
                }),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: SemanticTokensLegend {
                                token_types: vec![
                                    SemanticTokenType::FUNCTION,
                                    SemanticTokenType::TYPE,
                                    SemanticTokenType::STRUCT,
                                    SemanticTokenType::ENUM,
                                    SemanticTokenType::ENUM_MEMBER,
                                    SemanticTokenType::PARAMETER,
                                    SemanticTokenType::PROPERTY,
                                    SemanticTokenType::VARIABLE,
                                    SemanticTokenType::NAMESPACE,
                                    SemanticTokenType::TYPE_PARAMETER,
                                ],
                                token_modifiers: vec![
                                    SemanticTokenModifier::DECLARATION,
                                    SemanticTokenModifier::READONLY,
                                    SemanticTokenModifier::DEFAULT_LIBRARY,
                                ],
                            },
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: Some(false),
                            ..Default::default()
                        },
                    ),
                ),
                workspace: Some(WorkspaceServerCapabilities {
                    workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                        supported: Some(true),
                        change_notifications: Some(OneOf::Left(true)),
                    }),
                    file_operations: None,
                }),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "compact-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        tracing::info!("Server initialized - handshake complete");

        match self.diagnostic_engine.compiler_info().await {
            Ok(Some(info)) => {
                let message = format!(
                    "Compact compiler {} (language {})",
                    info.compiler_version, info.language_version
                );

                match info.compatibility {
                    CompilerCompatibility::Primary => {
                        tracing::info!("{}", message);
                        self.client.log_message(MessageType::INFO, message).await;
                    }
                    CompilerCompatibility::BestEffort => {
                        let message = format!(
                            "{}; Compact 0.32 support is best-effort (0.33 is the primary target)",
                            message
                        );
                        tracing::warn!("{}", message);
                        self.client.log_message(MessageType::WARNING, message).await;
                    }
                    CompilerCompatibility::Unsupported | CompilerCompatibility::Unknown => {
                        let message = format!(
                            "{}; this compiler is outside the Compact 0.33 compatibility target",
                            message
                        );
                        tracing::warn!("{}", message);
                        self.client.log_message(MessageType::WARNING, message).await;
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!("Could not query Compact compiler version: {}", error);
                self.client
                    .log_message(
                        MessageType::WARNING,
                        format!("Could not query Compact compiler version: {}", error),
                    )
                    .await;
            }
        }

        self.register_workspace_file_watcher().await;
        self.scan_workspace().await;
        self.client
            .log_message(MessageType::INFO, "Compact LSP server ready")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        tracing::info!("Shutdown requested");
        Ok(())
    }

    async fn did_change_workspace_folders(&self, params: DidChangeWorkspaceFoldersParams) {
        {
            let mut roots = self.workspace_roots.lock().unwrap();
            Self::apply_workspace_folder_change(&mut roots, params.event);
        }

        self.workspace_epoch.fetch_add(1, Ordering::AcqRel);
        self.scan_workspace().await;
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        self.workspace_epoch.fetch_add(1, Ordering::AcqRel);
        let mut affected_dependents = std::collections::HashSet::new();

        for change in params.changes {
            let uri = change.uri.to_string();
            if !self.workspace_contains_uri(&uri) {
                continue;
            }

            let dependents = if change.typ == FileChangeType::DELETED {
                if self.documents.contains_key(&uri) {
                    self.refresh_workspace_file(&uri)
                        .await
                        .map(|cache_uri| self.get_dependents(&cache_uri))
                        .unwrap_or_default()
                } else {
                    Self::normalized_file_uri(&uri)
                        .map(|cache_uri| self.remove_workspace_file(&cache_uri))
                        .unwrap_or_default()
                }
            } else {
                match self.refresh_workspace_file(&uri).await {
                    Some(cache_uri) => self.get_dependents(&cache_uri),
                    None => Self::normalized_file_uri(&uri)
                        .map(|cache_uri| self.remove_workspace_file(&cache_uri))
                        .unwrap_or_default(),
                }
            };
            affected_dependents.extend(dependents);
        }

        for dependent in affected_dependents {
            if !self.documents.contains_key(&dependent) {
                continue;
            }
            if let Ok(uri) = dependent.parse::<Uri>() {
                self.publish_diagnostics(uri).await;
            }
        }
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        tracing::debug!("Document opened: {}", uri);

        let rope = Rope::from_str(&params.text_document.text);
        self.documents.insert(
            uri.clone(),
            Document {
                content: rope,
                version: params.text_document.version,
            },
        );

        self.update_symbol_cache(&uri, &params.text_document.text);
        self.update_reverse_dependencies(&uri, &params.text_document.text);
        self.publish_diagnostics(params.text_document.uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let uri_string = uri.to_string();
        let version = params.text_document.version;

        if params
            .content_changes
            .iter()
            .any(|change| change.range.is_some())
        {
            tracing::warn!(
                "Ignoring incremental change for {} because the server negotiated full sync",
                uri_string
            );
            return;
        }

        let Some(change) = params.content_changes.last() else {
            tracing::warn!("Ignoring empty document change for {}", uri_string);
            return;
        };

        let updated = match self.documents.get_mut(&uri_string) {
            Some(mut document) => document.replace_if_newer(version, &change.text),
            None => {
                tracing::warn!("Ignoring change for unopened document {}", uri_string);
                return;
            }
        };

        if !updated {
            tracing::warn!(
                "Ignoring stale document change for {} at version {}",
                uri_string,
                version
            );
            return;
        }

        self.publish_syntax_diagnostics(uri.clone()).await;

        let content = match self.documents.get(&uri_string) {
            Some(doc) => doc.content.to_string(),
            None => return,
        };
        self.update_symbol_cache(&uri_string, &content);
        self.update_reverse_dependencies(&uri_string, &content);
        self.schedule_semantic_diagnostics(uri, content).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let uri_str = uri.to_string();

        if let Some(doc) = self.documents.get(&uri_str) {
            let content = doc.content.to_string();
            self.update_symbol_cache(&uri_str, &content);
            self.update_reverse_dependencies(&uri_str, &content);
        }

        self.publish_diagnostics(uri).await;

        let dependents = self.get_dependents(&uri_str);
        if !dependents.is_empty() {
            for dependent_uri in dependents {
                if let Ok(dep_uri) = dependent_uri.parse::<Uri>() {
                    self.publish_diagnostics(dep_uri).await;
                }
            }
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri.to_string();

        if let Some((_, handle)) = self.pending_diagnostics.remove(&uri) {
            handle.abort();
        }

        self.documents.remove(&uri);

        self.client
            .publish_diagnostics(params.text_document.uri, vec![], None)
            .await;

        if self.refresh_workspace_file(&uri).await.is_none() {
            if let Some(cache_uri) = Self::normalized_file_uri(&uri) {
                self.remove_workspace_file(&cache_uri);
            } else {
                self.remove_workspace_file(&uri);
            }
        }
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri.to_string();
        let position = params.text_document_position.position;

        // Handle dot-completion for built-in type methods.
        // Detect dot context via trigger character OR by scanning the text.
        let is_dot_trigger = params
            .context
            .as_ref()
            .and_then(|ctx| ctx.trigger_character.as_deref())
            == Some(".");

        if let Some(doc) = self.documents.get(&uri) {
            let content = doc.content.to_string();

            // Try trigger-character-based detection first, then text-based fallback
            let var_name = if is_dot_trigger {
                tracing::debug!("Dot completion: trigger character detected");
                utils::get_dot_access_variable(&content, position.line, position.character)
            } else {
                tracing::debug!("Dot completion: checking text context");
                utils::detect_dot_context(&content, position.line, position.character)
            };

            if let Some(var_name) = var_name {
                tracing::debug!("Dot completion: base variable = {}", var_name);
                let var_type = {
                    let mut parser = self.parser_engine.lock().unwrap();
                    parser.get_variable_type(&content, &var_name)
                };
                // Fallback: `kernel` is implicitly available without a ledger declaration
                let var_type = var_type.or_else(|| {
                    if var_name == "kernel" {
                        Some("Kernel".to_string())
                    } else {
                        None
                    }
                });
                if let Some(type_str) = var_type {
                    let base_type = builtins::extract_base_type(&type_str);
                    tracing::debug!("Dot completion: resolved type = {}", base_type);
                    let methods = builtins::methods_for_type(base_type);
                    if !methods.is_empty() {
                        let items: Vec<CompletionItem> = methods
                            .iter()
                            .map(|m| CompletionItem {
                                label: m.name.to_string(),
                                kind: Some(CompletionItemKind::METHOD),
                                detail: Some(m.signature.to_string()),
                                documentation: Some(Documentation::String(
                                    m.documentation.to_string(),
                                )),
                                insert_text: Some(m.snippet.to_string()),
                                insert_text_format: Some(InsertTextFormat::SNIPPET),
                                ..Default::default()
                            })
                            .collect();
                        return Ok(Some(CompletionResponse::Array(items)));
                    }
                }
            }
        }

        let mut items = Vec::new();

        fn symbol_to_lsp_kind(kind: compact_analyzer::CompletionSymbolKind) -> CompletionItemKind {
            use compact_analyzer::CompletionSymbolKind;
            match kind {
                CompletionSymbolKind::Function => CompletionItemKind::FUNCTION,
                CompletionSymbolKind::Struct => CompletionItemKind::STRUCT,
                CompletionSymbolKind::Enum => CompletionItemKind::ENUM,
                CompletionSymbolKind::Variable => CompletionItemKind::VARIABLE,
                CompletionSymbolKind::Module => CompletionItemKind::MODULE,
            }
        }

        let file_imports = if let Some(doc) = self.documents.get(&uri) {
            let content = doc.content.to_string();

            let symbols = {
                let mut parser = self.parser_engine.lock().unwrap();
                parser.get_completion_symbols(&content)
            };

            for sym in symbols {
                items.push(CompletionItem {
                    label: sym.name.clone(),
                    kind: Some(symbol_to_lsp_kind(sym.kind)),
                    detail: sym.detail,
                    insert_text: Some(sym.name),
                    ..Default::default()
                });
            }

            let file_imports = {
                let mut parser = self.parser_engine.lock().unwrap();
                parser.get_imports(&content)
            };
            file_imports
        } else {
            vec![]
        };

        for import in &file_imports {
            if !import.is_file {
                continue;
            }

            let resolved_uri = match imports::resolve_import_path(&uri, &import.path) {
                Some(uri) => uri,
                None => continue,
            };

            if let Some(entry) = self.symbol_cache.get(&resolved_uri) {
                let prefix = import.prefix.as_deref().unwrap_or("");

                for sym in entry.value().iter() {
                    let prefixed_name = if prefix.is_empty() {
                        sym.name.clone()
                    } else {
                        format!("{}{}", prefix, sym.name)
                    };

                    let source_file = import.path.rsplit('/').next().unwrap_or(&import.path);

                    items.push(CompletionItem {
                        label: prefixed_name.clone(),
                        kind: Some(symbol_to_lsp_kind(sym.kind)),
                        detail: Some(format!(
                            "{} (from {})",
                            sym.detail.as_deref().unwrap_or(""),
                            source_file
                        )),
                        insert_text: Some(prefixed_name),
                        ..Default::default()
                    });
                }
            }
        }

        // Keywords
        let keywords = [
            ("pragma", "Version pragma declaration"),
            ("import", "Import a module"),
            ("export", "Export a declaration"),
            ("module", "Define a module"),
            ("include", "Include a file"),
            ("ledger", "Declare ledger state"),
            ("circuit", "Define a circuit function"),
            ("witness", "Declare a witness function"),
            ("contract", "Declare an external contract"),
            ("struct", "Define a struct type"),
            ("enum", "Define an enum type"),
            ("constructor", "Define a constructor"),
            ("return", "Return from function"),
            ("if", "Conditional statement"),
            ("else", "Else branch"),
            ("for", "For loop"),
            ("of", "Iterator/range keyword"),
            ("assert", "Assertion with error message"),
            ("const", "Constant declaration"),
            ("default", "Default value"),
            ("map", "Map over values"),
            ("fold", "Fold/reduce values"),
            ("disclose", "Disclose a value"),
            ("pad", "Pad a string"),
            ("as", "Type cast"),
            ("pure", "Pure function modifier"),
            ("sealed", "Sealed ledger modifier"),
            ("prefix", "Import prefix"),
        ];

        for (keyword, detail) in keywords {
            items.push(CompletionItem {
                label: keyword.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: Some(detail.to_string()),
                insert_text: Some(keyword.to_string()),
                ..Default::default()
            });
        }

        // Built-in types
        let types = [
            ("Boolean", "Boolean type (true/false)"),
            ("Field", "Field arithmetic type"),
            ("Uint", "Unsigned integer with bit size"),
            ("Bytes", "Fixed-size byte array"),
            ("Opaque", "Opaque type wrapper"),
            ("Vector", "Fixed-size vector"),
            ("Counter", "Atomic counter for ledger state"),
            ("Map", "Key-value mapping for ledger state"),
            ("Set", "Set collection for ledger state"),
            ("Cell", "Mutable cell for ledger state"),
            ("List", "Ordered list for ledger state"),
            ("MerkleTree", "Merkle tree for ledger state"),
            (
                "HistoricMerkleTree",
                "Historic Merkle tree with root history",
            ),
            ("Kernel", "Built-in kernel operations"),
            ("Address", "Blockchain address type"),
            ("Void", "Void return type"),
        ];

        for (type_name, detail) in types {
            items.push(CompletionItem {
                label: type_name.to_string(),
                kind: Some(CompletionItemKind::TYPE_PARAMETER),
                detail: Some(detail.to_string()),
                insert_text: Some(type_name.to_string()),
                ..Default::default()
            });
        }

        // Type snippets
        let type_snippets = [
            (
                "Uint<>",
                "Uint<${1:32}>",
                "Unsigned integer (e.g., Uint<32>)",
            ),
            ("Bytes<>", "Bytes<${1:32}>", "Byte array (e.g., Bytes<32>)"),
            (
                "Vector<>",
                "Vector<${1:10}, ${2:Field}>",
                "Vector (e.g., Vector<10, Field>)",
            ),
            (
                "Opaque<>",
                "Opaque<\"${1:name}\">",
                "Opaque type (e.g., Opaque<\"mytype\">)",
            ),
            (
                "Map<>",
                "Map<${1:Key}, ${2:Value}>",
                "Map (e.g., Map<Address, Uint<64>>)",
            ),
            ("Set<>", "Set<${1:T}>", "Set (e.g., Set<Address>)"),
            ("Cell<>", "Cell<${1:T}>", "Cell (e.g., Cell<Field>)"),
            ("List<>", "List<${1:T}>", "List (e.g., List<Field>)"),
            (
                "MerkleTree<>",
                "MerkleTree<${1:32}, ${2:Field}>",
                "Merkle tree (e.g., MerkleTree<32, Bytes<32>>)",
            ),
            (
                "HistoricMerkleTree<>",
                "HistoricMerkleTree<${1:32}, ${2:Field}>",
                "Historic Merkle tree",
            ),
        ];

        for (label, snippet, detail) in type_snippets {
            items.push(CompletionItem {
                label: label.to_string(),
                kind: Some(CompletionItemKind::SNIPPET),
                detail: Some(detail.to_string()),
                insert_text: Some(snippet.to_string()),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            });
        }

        // Stdlib struct types
        for st in stdlib::all_stdlib_structs() {
            items.push(CompletionItem {
                label: st.name.to_string(),
                kind: Some(CompletionItemKind::STRUCT),
                detail: Some(st.description.to_string()),
                insert_text: Some(st.name.to_string()),
                ..Default::default()
            });
        }

        // Stdlib parameterized type snippets
        let stdlib_type_snippets = [
            ("Maybe<>", "Maybe<${1:T}>", "Optional container (Maybe<T>)"),
            (
                "Either<>",
                "Either<${1:A}, ${2:B}>",
                "Union type (Either<A, B>)",
            ),
            (
                "MerkleTreePath<>",
                "MerkleTreePath<${1:n}, ${2:T}>",
                "Merkle tree path proof",
            ),
        ];

        for (label, snippet, detail) in stdlib_type_snippets {
            items.push(CompletionItem {
                label: label.to_string(),
                kind: Some(CompletionItemKind::SNIPPET),
                detail: Some(detail.to_string()),
                insert_text: Some(snippet.to_string()),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            });
        }

        // Stdlib circuit functions
        for circ in stdlib::all_stdlib_circuits() {
            items.push(CompletionItem {
                label: circ.name.to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some(circ.signature.to_string()),
                documentation: Some(Documentation::String(circ.doc.to_string())),
                insert_text: Some(circ.snippet.to_string()),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            });
        }

        // Boolean literals
        items.push(CompletionItem {
            label: "true".to_string(),
            kind: Some(CompletionItemKind::CONSTANT),
            detail: Some("Boolean true".to_string()),
            ..Default::default()
        });
        items.push(CompletionItem {
            label: "false".to_string(),
            kind: Some(CompletionItemKind::CONSTANT),
            detail: Some("Boolean false".to_string()),
            ..Default::default()
        });

        // Code snippets
        let snippets = [
            // Circuit snippets
            (
                "circuit",
                "circuit snippet",
                "circuit ${1:name}(${2:params}): ${3:ReturnType} {\n\t$0\n}",
                "Circuit function template",
            ),
            (
                "export circuit",
                "export circuit snippet",
                "export circuit ${1:name}(${2:params}): ${3:ReturnType} {\n\t$0\n}",
                "Exported circuit function template",
            ),
            (
                "pure circuit",
                "pure circuit snippet",
                "pure circuit ${1:name}(${2:params}): ${3:ReturnType} {\n\t$0\n}",
                "Pure circuit function template",
            ),
            (
                "export pure circuit",
                "export pure circuit snippet",
                "export pure circuit ${1:name}(${2:params}): ${3:ReturnType} {\n\t$0\n}",
                "Exported pure circuit function template",
            ),
            // Struct snippets
            (
                "struct",
                "struct snippet",
                "struct ${1:Name} {\n\t${2:field}: ${3:Type};\n}",
                "Struct definition template",
            ),
            (
                "export struct",
                "export struct snippet",
                "export struct ${1:Name} {\n\t${2:field}: ${3:Type};\n}",
                "Exported struct definition template",
            ),
            // Enum snippets
            (
                "enum",
                "enum snippet",
                "enum ${1:Name} {\n\t${2:Variant1},\n\t${3:Variant2},\n}",
                "Enum definition template",
            ),
            (
                "export enum",
                "export enum snippet",
                "export enum ${1:Name} {\n\t${2:Variant1},\n\t${3:Variant2},\n}",
                "Exported enum definition template",
            ),
            // Ledger snippets
            (
                "ledger",
                "ledger snippet",
                "ledger ${1:name}: ${2:Type};",
                "Ledger declaration template",
            ),
            (
                "export ledger",
                "export ledger snippet",
                "export ledger ${1:name}: ${2:Type};",
                "Exported ledger declaration template",
            ),
            (
                "sealed ledger",
                "sealed ledger snippet",
                "sealed ledger ${1:name}: ${2:Type};",
                "Sealed ledger declaration template",
            ),
            (
                "export sealed ledger",
                "export sealed ledger snippet",
                "export sealed ledger ${1:name}: ${2:Type};",
                "Exported sealed ledger declaration template",
            ),
            // Witness snippets
            (
                "witness",
                "witness snippet",
                "witness ${1:name}(${2:params}): ${3:ReturnType};",
                "Witness declaration template",
            ),
            (
                "export witness",
                "export witness snippet",
                "export witness ${1:name}(${2:params}): ${3:ReturnType};",
                "Exported witness declaration template",
            ),
            // Contract snippets
            (
                "contract",
                "contract snippet",
                "contract ${1:Name} {\n\tcircuit ${2:fn}(${3:params}): ${4:ReturnType};\n}",
                "External contract declaration template",
            ),
            (
                "export contract",
                "export contract snippet",
                "export contract ${1:Name} {\n\tcircuit ${2:fn}(${3:params}): ${4:ReturnType};\n}",
                "Exported contract declaration template",
            ),
            // Module snippets
            (
                "module",
                "module snippet",
                "module ${1:Name} {\n\t$0\n}",
                "Module definition template",
            ),
            (
                "export module",
                "export module snippet",
                "export module ${1:Name} {\n\t$0\n}",
                "Exported module definition template",
            ),
            // Other declaration snippets
            (
                "constructor",
                "constructor snippet",
                "constructor(${1:params}) {\n\t$0\n}",
                "Constructor template",
            ),
            (
                "const",
                "const snippet",
                "const ${1:name}: ${2:Type} = ${3:value};",
                "Constant declaration template",
            ),
            (
                "include",
                "include snippet",
                "include \"${1:path}\";",
                "File inclusion template",
            ),
            // Import snippets
            (
                "import",
                "import snippet",
                "import ${1:Module};",
                "Import module template",
            ),
            (
                "import file",
                "import file snippet",
                "import \"${1:path}\";",
                "Import file template",
            ),
            (
                "import prefix",
                "import prefix snippet",
                "import ${1:Module} prefix ${2:alias};",
                "Import with prefix alias template",
            ),
            // Statement snippets
            (
                "if",
                "if snippet",
                "if (${1:condition}) {\n\t$0\n}",
                "If statement template",
            ),
            (
                "if-else",
                "if-else snippet",
                "if (${1:condition}) {\n\t$2\n} else {\n\t$0\n}",
                "If-else statement template",
            ),
            (
                "for",
                "for snippet",
                "for (const ${1:i} of ${2:0}..${3:10}) {\n\t$0\n}",
                "For loop template",
            ),
            (
                "assert",
                "assert snippet",
                "assert ${1:condition} \"${2:error message}\";",
                "Assertion template",
            ),
            // Pragma snippet
            (
                "pragma",
                "pragma snippet",
                "pragma language_version ${1:>=0.14.0};",
                "Pragma declaration template",
            ),
        ];

        for (label, filter, snippet, detail) in snippets {
            items.push(CompletionItem {
                label: label.to_string(),
                kind: Some(CompletionItemKind::SNIPPET),
                detail: Some(detail.to_string()),
                filter_text: Some(filter.to_string()),
                insert_text: Some(snippet.to_string()),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            });
        }

        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        if !Self::quick_fixes_requested(&params) {
            return Ok(Some(Vec::new()));
        }

        let actions = params
            .context
            .diagnostics
            .iter()
            .filter_map(|diagnostic| {
                Self::quick_fix_for_missing_token(&params.text_document.uri, diagnostic)
            })
            .collect();

        Ok(Some(actions))
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri.to_string();

        let content = match self.documents.get(&uri) {
            Some(doc) => doc.content.to_string(),
            None => return Ok(None),
        };

        let formatted = match self.formatter_engine.format(&content).await {
            Ok(formatted) => formatted,
            Err(e) => {
                self.client
                    .show_message(MessageType::ERROR, format!("Formatting failed: {}", e))
                    .await;
                return Ok(None);
            }
        };

        if formatted == content {
            return Ok(Some(vec![]));
        }

        let range = Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: utils::document_end_position(&content),
        };

        Ok(Some(vec![TextEdit {
            range,
            new_text: formatted,
        }]))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri.to_string();

        let content = match self.documents.get(&uri) {
            Some(doc) => doc.content.to_string(),
            None => return Ok(None),
        };

        let symbols = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser.document_symbols(&content)
        };

        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        let uri = params.text_document.uri.to_string();

        let content = match self.documents.get(&uri) {
            Some(doc) => doc.content.to_string(),
            None => return Ok(None),
        };

        let ranges = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser.folding_ranges(&content)
        };

        Ok(Some(ranges))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .to_string();
        let position = params.text_document_position_params.position;

        let content = match self.documents.get(&uri) {
            Some(doc) => doc.content.to_string(),
            None => return Ok(None),
        };

        let hover_info = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser.hover_info(&content, position.line, position.character)
        };

        if let Some(info) = hover_info {
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: info.content,
                }),
                range: info.range,
            }));
        }

        // Check for member access (e.g., hovering on "increment" in "round.increment(1)")
        let member_ctx = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser.get_member_access_context(&content, position.line, position.character)
        };
        if let Some(ctx) = member_ctx {
            let var_type = {
                let mut parser = self.parser_engine.lock().unwrap();
                parser.get_variable_type(&content, &ctx.base_name)
            };
            // Fallback: `kernel` is implicitly available without a ledger declaration
            let var_type = var_type.or_else(|| {
                if ctx.base_name == "kernel" {
                    Some("Kernel".to_string())
                } else {
                    None
                }
            });
            if let Some(type_str) = var_type {
                let base_type = builtins::extract_base_type(&type_str);
                if let Some(method) = builtins::find_method_by_name(base_type, &ctx.member_name) {
                    let hover_text = format!(
                        "```compact\n{}.{}  (on {})\n```\n\n{}",
                        ctx.base_name, method.signature, type_str, method.documentation
                    );
                    return Ok(Some(Hover {
                        contents: HoverContents::Markup(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: hover_text,
                        }),
                        range: Some(ctx.member_range),
                    }));
                }
            }
        }

        let word = match utils::get_word_at_position(&content, position.line, position.character) {
            Some(w) => w,
            None => return Ok(None),
        };

        if let Some((_file_uri, symbol)) = self.find_imported_symbol(&uri, &word) {
            let content = symbol.documentation.unwrap_or_else(|| {
                format!(
                    "```compact\n{}{}\n```",
                    symbol.name,
                    symbol.detail.as_deref().unwrap_or("")
                )
            });
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: content,
                }),
                range: None,
            }));
        }

        // Stdlib circuit hover (e.g., hovering on "send")
        if let Some(circ) = stdlib::find_stdlib_circuit(&word) {
            let mut hover_text = format!(
                "```compact\ncircuit {}\n```\n\n{}",
                circ.signature, circ.doc
            );
            if !circ.doc_url.is_empty() {
                hover_text.push_str(&format!(
                    "\n\n> [Compact Standard Library]({})",
                    circ.doc_url
                ));
            }
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: hover_text,
                }),
                range: None,
            }));
        }

        // Stdlib struct hover (e.g., hovering on "CoinInfo", "Maybe")
        let base_type = builtins::extract_base_type(&word);
        if let Some(st) = stdlib::find_stdlib_struct(base_type) {
            let mut hover_text = if st.type_params.is_empty() {
                format!("**{}**\n\n{}\n\n", st.name, st.description)
            } else {
                format!(
                    "**{}{}**\n\n{}\n\n",
                    st.name, st.type_params, st.description
                )
            };
            hover_text.push_str("**Fields:**\n");
            for field in &st.fields {
                hover_text.push_str(&format!(
                    "- `{}: {}` — {}\n",
                    field.name, field.type_str, field.doc
                ));
            }
            if !st.doc_url.is_empty() {
                hover_text.push_str(&format!("\n> [Compact Standard Library]({})", st.doc_url));
            }
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: hover_text,
                }),
                range: None,
            }));
        }

        Ok(None)
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .clone();
        let uri_string = uri.to_string();
        let position = params.text_document_position_params.position;

        let content = match self.documents.get(&uri_string) {
            Some(doc) => doc.content.to_string(),
            None => return Ok(None),
        };

        let def_location = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser.goto_definition(&content, position.line, position.character)
        };

        if let Some(loc) = def_location {
            return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                uri,
                range: loc.selection_range,
            })));
        }

        // Check for member access on built-in types (e.g., "increment" in "round.increment()")
        let member_ctx = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser.get_member_access_context(&content, position.line, position.character)
        };
        if let Some(ctx) = member_ctx {
            let var_type = {
                let mut parser = self.parser_engine.lock().unwrap();
                parser.get_variable_type(&content, &ctx.base_name)
            };
            // Fallback: `kernel` is implicitly available without a ledger declaration
            let var_type = var_type.or_else(|| {
                if ctx.base_name == "kernel" {
                    Some("Kernel".to_string())
                } else {
                    None
                }
            });
            if let Some(type_str) = var_type {
                let base_type = builtins::extract_base_type(&type_str);
                if let Some(doc_loc) =
                    builtins::get_builtin_method_doc_location(base_type, &ctx.member_name)
                {
                    let target_uri = match Uri::from_str(&doc_loc.uri) {
                        Ok(u) => u,
                        Err(_) => return Ok(None),
                    };
                    return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                        uri: target_uri,
                        range: Range {
                            start: Position {
                                line: doc_loc.line,
                                character: 0,
                            },
                            end: Position {
                                line: doc_loc.line,
                                character: 0,
                            },
                        },
                    })));
                }
            }
        }

        let word = match utils::get_word_at_position(&content, position.line, position.character) {
            Some(w) => w,
            None => return Ok(None),
        };

        if let Some((file_uri, symbol)) = self.find_imported_symbol(&uri_string, &word) {
            if let Some(loc) = symbol.location {
                let target_uri = match Uri::from_str(&file_uri) {
                    Ok(u) => u,
                    Err(_) => return Ok(None),
                };
                return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                    uri: target_uri,
                    range: Range {
                        start: Position {
                            line: loc.start_line,
                            character: loc.start_char,
                        },
                        end: Position {
                            line: loc.end_line,
                            character: loc.end_char,
                        },
                    },
                })));
            }
        }

        // Built-in type name lookup (e.g., "Counter" in "ledger round: Counter;")
        let base_type = builtins::extract_base_type(&word);
        if let Some(doc_loc) = builtins::get_builtin_type_doc_location(base_type) {
            let target_uri = match Uri::from_str(&doc_loc.uri) {
                Ok(u) => u,
                Err(_) => return Ok(None),
            };
            return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                uri: target_uri,
                range: Range {
                    start: Position {
                        line: doc_loc.line,
                        character: 0,
                    },
                    end: Position {
                        line: doc_loc.line,
                        character: 0,
                    },
                },
            })));
        }

        // Stdlib struct type lookup (e.g., "CoinInfo", "Maybe<T>")
        if let Some(doc_loc) = stdlib::get_stdlib_struct_doc_location(base_type) {
            let target_uri = match Uri::from_str(&doc_loc.uri) {
                Ok(u) => u,
                Err(_) => return Ok(None),
            };
            return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                uri: target_uri,
                range: Range {
                    start: Position {
                        line: doc_loc.line,
                        character: 0,
                    },
                    end: Position {
                        line: doc_loc.line,
                        character: 0,
                    },
                },
            })));
        }

        // Stdlib circuit function lookup (e.g., "send", "transientHash")
        if let Some(doc_loc) = stdlib::get_stdlib_circuit_doc_location(&word) {
            let target_uri = match Uri::from_str(&doc_loc.uri) {
                Ok(u) => u,
                Err(_) => return Ok(None),
            };
            return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                uri: target_uri,
                range: Range {
                    start: Position {
                        line: doc_loc.line,
                        character: 0,
                    },
                    end: Position {
                        line: doc_loc.line,
                        character: 0,
                    },
                },
            })));
        }

        Ok(None)
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .to_string();
        let position = params.text_document_position_params.position;

        let content = match self.documents.get(&uri) {
            Some(doc) => doc.content.to_string(),
            None => return Ok(None),
        };

        let sig_info = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser.signature_help(&content, position.line, position.character)
        };

        if let Some(info) = sig_info {
            return Ok(Some(self.build_signature_help_response(info)));
        }

        let func_name =
            match utils::get_function_call_name(&content, position.line, position.character) {
                Some(name) => name,
                None => return Ok(None),
            };

        if let Some((_file_uri, symbol)) = self.find_imported_symbol(&uri, &func_name) {
            if let Some(detail) = &symbol.detail {
                let active_param =
                    utils::count_commas_before_cursor(&content, position.line, position.character);
                let params = utils::parse_params_from_detail(detail);

                let parameters: Vec<ParameterInformation> = params
                    .iter()
                    .map(|p| ParameterInformation {
                        label: ParameterLabel::Simple(p.clone()),
                        documentation: None,
                    })
                    .collect();

                let label = format!("circuit {}{}", func_name, detail);
                let signature = SignatureInformation {
                    label,
                    documentation: symbol.documentation.map(|d| {
                        Documentation::MarkupContent(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: d,
                        })
                    }),
                    parameters: Some(parameters),
                    active_parameter: Some(active_param),
                };

                return Ok(Some(SignatureHelp {
                    signatures: vec![signature],
                    active_signature: Some(0),
                    active_parameter: Some(active_param),
                }));
            }
        }

        // Stdlib circuit function signature help
        if let Some(circ) = stdlib::find_stdlib_circuit(&func_name) {
            let active_param =
                utils::count_commas_before_cursor(&content, position.line, position.character);
            // Build detail string from signature: "send(input: QualifiedCoinInfo, ...): SendResult" → "(input: QualifiedCoinInfo, ...): SendResult"
            let detail = circ
                .signature
                .find('(')
                .map(|i| &circ.signature[i..])
                .unwrap_or(circ.signature);
            let params = utils::parse_params_from_detail(detail);

            let parameters: Vec<ParameterInformation> = params
                .iter()
                .map(|p| ParameterInformation {
                    label: ParameterLabel::Simple(p.clone()),
                    documentation: None,
                })
                .collect();

            let label = format!("circuit {}", circ.signature);
            let signature = SignatureInformation {
                label,
                documentation: Some(Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: circ.doc.to_string(),
                })),
                parameters: Some(parameters),
                active_parameter: Some(active_param),
            };

            return Ok(Some(SignatureHelp {
                signatures: vec![signature],
                active_signature: Some(0),
                active_parameter: Some(active_param),
            }));
        }

        Ok(None)
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri.to_string();

        let content = match self.documents.get(&uri) {
            Some(doc) => doc.content.to_string(),
            None => return Ok(None),
        };

        let tokens = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser.get_semantic_tokens(&content)
        };

        let mut data = Vec::new();
        let mut prev_line = 0u32;
        let mut prev_char = 0u32;

        for token in tokens {
            let line = token.range.start.line;
            let char = token.range.start.character;
            let length = token
                .range
                .end
                .character
                .saturating_sub(token.range.start.character);

            let delta_line = line - prev_line;
            let delta_start = if delta_line == 0 {
                char - prev_char
            } else {
                char
            };

            let mut modifier_mask = 0u32;
            for modifier in &token.modifiers {
                modifier_mask |= 1 << (*modifier as u32);
            }

            data.push(lsp_types::SemanticToken {
                delta_line,
                delta_start,
                length,
                token_type: token.token_type as u32,
                token_modifiers_bitset: modifier_mask,
            });

            prev_line = line;
            prev_char = char;
        }

        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        })))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri.clone();
        let uri_string = uri.to_string();
        let position = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;

        let content = match self.documents.get(&uri_string) {
            Some(doc) => doc.content.to_string(),
            None => return Ok(None),
        };

        let symbol_name =
            match utils::get_word_at_position(&content, position.line, position.character) {
                Some(name) => name,
                None => return Ok(None),
            };
        let cache_uri = Self::cache_uri(&uri_string);

        let mut all_locations = Vec::new();

        let local_refs = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser.find_references(&content, &symbol_name)
        };

        for r in local_refs {
            if r.is_definition && !include_declaration {
                continue;
            }
            all_locations.push(Location {
                uri: uri.clone(),
                range: r.range,
            });
        }

        for entry in self.source_cache.iter() {
            let file_uri = entry.key();
            if file_uri == &cache_uri {
                continue;
            }

            let file_content = entry.value();
            let search_names = self.get_search_names_for_file(file_uri, &cache_uri, &symbol_name);

            for search_name in search_names {
                let refs = {
                    let mut parser = self.parser_engine.lock().unwrap();
                    parser.find_references(file_content, &search_name)
                };

                for r in refs {
                    if r.is_definition {
                        continue;
                    }
                    if let Ok(loc_uri) = file_uri.parse::<lsp_types::Uri>() {
                        all_locations.push(Location {
                            uri: loc_uri,
                            range: r.range,
                        });
                    }
                }
            }
        }

        if all_locations.is_empty() {
            Ok(None)
        } else {
            Ok(Some(all_locations))
        }
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let uri = params.text_document.uri.to_string();
        let position = params.position;

        let content = match self.documents.get(&uri) {
            Some(doc) => doc.content.to_string(),
            None => return Ok(None),
        };

        let symbol_name =
            match utils::get_word_at_position(&content, position.line, position.character) {
                Some(name) => name,
                None => return Ok(None),
            };

        if validation::is_keyword(&symbol_name) || validation::is_builtin_type(&symbol_name) {
            return Ok(None);
        }

        let range =
            match utils::get_word_range_at_position(&content, position.line, position.character) {
                Some(r) => r,
                None => return Ok(None),
            };

        Ok(Some(PrepareRenameResponse::Range(range)))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri.clone();
        let uri_string = uri.to_string();
        let position = params.text_document_position.position;
        let new_name = params.new_name;

        let content = match self.documents.get(&uri_string) {
            Some(doc) => doc.content.to_string(),
            None => return Ok(None),
        };

        let old_name =
            match utils::get_word_at_position(&content, position.line, position.character) {
                Some(name) => name,
                None => return Ok(None),
            };
        let cache_uri = Self::cache_uri(&uri_string);

        if !validation::is_valid_identifier(&new_name) {
            return Err(tower_lsp::jsonrpc::Error::invalid_params(
                "Invalid identifier: must start with letter or underscore",
            ));
        }

        if validation::is_keyword(&new_name) {
            return Err(tower_lsp::jsonrpc::Error::invalid_params(
                "Cannot rename to a keyword",
            ));
        }

        if validation::is_builtin_type(&new_name) {
            return Err(tower_lsp::jsonrpc::Error::invalid_params(
                "Cannot rename to a built-in type name",
            ));
        }

        let mut changes: std::collections::HashMap<lsp_types::Uri, Vec<TextEdit>> =
            std::collections::HashMap::new();

        let local_refs = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser.find_references(&content, &old_name)
        };

        for r in local_refs {
            changes.entry(uri.clone()).or_default().push(TextEdit {
                range: r.range,
                new_text: new_name.clone(),
            });
        }

        for entry in self.source_cache.iter() {
            let file_uri = entry.key();
            if file_uri == &cache_uri {
                continue;
            }

            let file_content = entry.value();
            let search_names = self.get_search_names_for_file(file_uri, &cache_uri, &old_name);

            for search_name in search_names {
                let refs = {
                    let mut parser = self.parser_engine.lock().unwrap();
                    parser.find_references(file_content, &search_name)
                };

                let new_name_for_file = if search_name != old_name {
                    let prefix = &search_name[..search_name.len() - old_name.len()];
                    format!("{}{}", prefix, new_name)
                } else {
                    new_name.clone()
                };

                for r in refs {
                    if r.is_definition {
                        continue;
                    }
                    if let Ok(loc_uri) = file_uri.parse::<lsp_types::Uri>() {
                        changes.entry(loc_uri).or_default().push(TextEdit {
                            range: r.range,
                            new_text: new_name_for_file.clone(),
                        });
                    }
                }
            }
        }

        if changes.is_empty() {
            return Ok(None);
        }

        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }))
    }
}

#[cfg(test)]
mod workspace_tests {
    use super::*;

    #[test]
    fn indexes_multiple_and_nested_workspace_roots_without_duplicates() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let contracts = first.path().join("contracts");
        std::fs::create_dir_all(&contracts).unwrap();

        std::fs::write(
            contracts.join("Utility.compact"),
            "circuit utility(): Field { return 1; }",
        )
        .unwrap();
        std::fs::write(
            contracts.join("Main.compact"),
            "import \"./Utility\";\ncircuit main(): Field { return utility(); }",
        )
        .unwrap();
        std::fs::write(
            second.path().join("Other.compact"),
            "circuit other(): Field { return 2; }",
        )
        .unwrap();

        let roots = vec![
            imports::path_to_file_uri(first.path()).unwrap(),
            imports::path_to_file_uri(&contracts).unwrap(),
            imports::path_to_file_uri(second.path()).unwrap(),
        ];
        let indexed = CompactLanguageServer::index_workspace_roots(roots);

        assert_eq!(indexed.len(), 3);
        assert!(indexed
            .iter()
            .any(|file| { file.symbols.iter().any(|symbol| symbol.name == "utility") }));
        assert!(indexed.iter().any(
            |file| file.symbols.iter().any(|symbol| symbol.name == "main")
                && file.imports.iter().any(|import| import.path == "./Utility")
        ));
        assert!(indexed
            .iter()
            .any(|file| file.symbols.iter().any(|symbol| symbol.name == "other")));
    }

    #[test]
    fn workspace_folder_changes_remove_add_and_deduplicate_roots() {
        let first = Uri::from_str("file:///workspace/first").unwrap();
        let second = Uri::from_str("file:///workspace/second").unwrap();
        let third = Uri::from_str("file:///workspace/third").unwrap();
        let mut roots = vec![first.to_string(), second.to_string()];

        CompactLanguageServer::apply_workspace_folder_change(
            &mut roots,
            WorkspaceFoldersChangeEvent {
                added: vec![
                    WorkspaceFolder {
                        uri: second.clone(),
                        name: "second".into(),
                    },
                    WorkspaceFolder {
                        uri: third.clone(),
                        name: "third".into(),
                    },
                ],
                removed: vec![WorkspaceFolder {
                    uri: first,
                    name: "first".into(),
                }],
            },
        );

        assert_eq!(roots, vec![second.to_string(), third.to_string()]);
    }
}

#[cfg(test)]
mod code_action_tests {
    use super::*;

    fn diagnostic(message: &str, source: &str) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position {
                    line: 2,
                    character: 12,
                },
                end: Position {
                    line: 2,
                    character: 12,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some(source.to_string()),
            message: message.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn creates_preferred_quick_fix_for_missing_punctuation() {
        let uri = Uri::from_str("file:///workspace/Main.compact").unwrap();
        let diagnostic = diagnostic("Syntax error: missing ;", "compact-syntax");

        let action = CompactLanguageServer::quick_fix_for_missing_token(&uri, &diagnostic)
            .expect("missing punctuation should have a quick fix");
        let CodeActionOrCommand::CodeAction(action) = action else {
            panic!("expected a code action");
        };

        assert_eq!(action.title, "Insert missing `;`");
        assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));
        assert_eq!(action.is_preferred, Some(true));
        assert_eq!(action.diagnostics, Some(vec![diagnostic.clone()]));
        assert_eq!(
            action.edit.unwrap().changes.unwrap().get(&uri).unwrap(),
            &vec![TextEdit {
                range: diagnostic.range,
                new_text: ";".to_string(),
            }]
        );
    }

    #[test]
    fn ignores_unsafe_or_unrelated_diagnostics() {
        let uri = Uri::from_str("file:///workspace/Main.compact").unwrap();

        assert!(CompactLanguageServer::quick_fix_for_missing_token(
            &uri,
            &diagnostic("Syntax error: missing identifier", "compact-syntax")
        )
        .is_none());
        assert!(CompactLanguageServer::quick_fix_for_missing_token(
            &uri,
            &diagnostic("Syntax error: missing ;", "compactc")
        )
        .is_none());
    }
}
