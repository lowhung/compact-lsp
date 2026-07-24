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

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use compact_analyzer::{
    CallHierarchyDocument, CircuitDefinition, CompilerCompatibility, CompletionSymbol,
    DiagnosticEngine, FormatterEngine, ImportInfo, ParserEngine,
};
use dashmap::DashMap;
use lsp_types::*;
use ropey::Rope;
use tokio::sync::Mutex as AsyncMutex;
use tower_lsp::jsonrpc::Result;
use tower_lsp::{Client, LanguageServer};

/// One disk-backed file prepared off the async runtime before cache replacement.
struct IndexedWorkspaceFile {
    /// Canonical file URI used as the key in every workspace cache.
    uri: String,
    /// Source retained for cross-file hover, references, and rename.
    content: String,
    /// Declarations retained for completion and workspace-symbol queries.
    symbols: Vec<CompletionSymbol>,
    /// Imports used to rebuild reverse dependency edges after the scan.
    imports: Vec<ImportInfo>,
}

/// One cached document parsed for a call-hierarchy request.
struct CallHierarchyFile {
    /// Canonical cache URI used for import resolution and result identity.
    uri: String,
    /// Circuits, direct calls, and imports from one syntax-tree snapshot.
    document: CallHierarchyDocument,
}

/// A sortable workspace-symbol result plus the stable keys used to rank and deduplicate it.
struct WorkspaceSymbolCandidate {
    /// Query match quality: exact, prefix, substring, or an unfiltered empty-query match.
    match_rank: u8,
    /// Lowercase symbol name used for case-insensitive matching and ordering.
    normalized_name: String,
    /// Stable secondary ordering for Compact declaration kinds.
    kind_rank: u8,
    /// Canonical document URI used to keep multi-root results deterministic.
    uri: String,
    /// LSP payload returned to the client after sorting and deduplication.
    information: SymbolInformation,
}

/// Per-request declaration snapshot used to resolve inlay-hint signatures.
///
/// `None` marks an ambiguous or incomplete local function signature, allowing
/// the resolver to distinguish it from a name that is absent locally. Ledger
/// types come from the same analyzer pass, avoiding a reparse for every call.
struct InlaySignatureIndex {
    local_functions: HashMap<String, Option<String>>,
    ledger_types: HashMap<String, String>,
}

/// One debounced semantic-diagnostic task tracked for an open document.
///
/// The generation prevents a task that finishes concurrently with replacement
/// from removing or publishing over the newer task.
struct PendingDiagnosticTask {
    generation: u64,
    handle: tokio::task::JoinHandle<()>,
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

    /// Pending semantic diagnostics tasks, keyed by document URI.
    pending_diagnostics: Arc<DashMap<String, PendingDiagnosticTask>>,

    /// Monotonic identity for distinguishing replaced diagnostic tasks.
    next_diagnostic_generation: Arc<AtomicU64>,

    /// Reverse dependency map for cross-file error propagation.
    reverse_dependencies: Arc<DashMap<String, Vec<String>>>,
}

impl CompactLanguageServer {
    /// Build a request-local call graph from the current source-cache snapshot.
    ///
    /// A private parser avoids holding the shared interactive parser mutex while
    /// scanning the workspace. Watched-file and open-document updates replace
    /// `source_cache`, so requests use the latest cached snapshot available for
    /// each file.
    fn call_hierarchy_files(&self) -> Vec<CallHierarchyFile> {
        let mut sources = self
            .source_cache
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect::<Vec<_>>();
        sources.sort_by(|left, right| left.0.cmp(&right.0));

        let mut parser = ParserEngine::new();
        sources
            .into_iter()
            .map(|(uri, source)| CallHierarchyFile {
                uri,
                document: parser.call_hierarchy(&source),
            })
            .collect()
    }

    /// Resolve a source-level call name to exactly one local or imported circuit.
    ///
    /// Local declarations and file imports are considered together. Duplicate
    /// declarations, colliding unprefixed imports, and any other ambiguity return
    /// `None` instead of inventing a semantic edge.
    fn resolve_call_target<'a>(
        files: &'a [CallHierarchyFile],
        caller: &'a CallHierarchyFile,
        rendered_name: &str,
    ) -> Option<(&'a CallHierarchyFile, &'a CircuitDefinition)> {
        let mut candidates = caller
            .document
            .circuits
            .iter()
            .filter(|circuit| circuit.name == rendered_name)
            .map(|circuit| (caller, circuit))
            .collect::<Vec<_>>();

        for import in &caller.document.imports {
            if !import.is_file {
                continue;
            }
            let target_name = match import.prefix.as_deref() {
                Some(prefix) => match rendered_name.strip_prefix(prefix) {
                    Some(name) if !name.is_empty() => name,
                    _ => continue,
                },
                None => rendered_name,
            };
            let Some(import_uri) = imports::resolve_import_path(&caller.uri, &import.path) else {
                continue;
            };
            let import_uri = Self::cache_uri(&import_uri);
            let Some(imported_file) = files.iter().find(|file| file.uri == import_uri) else {
                continue;
            };
            candidates.extend(
                imported_file
                    .document
                    .circuits
                    .iter()
                    .filter(|circuit| circuit.name == target_name)
                    .map(|circuit| (imported_file, circuit)),
            );
        }

        candidates.sort_by(|(left_file, left), (right_file, right)| {
            left_file
                .uri
                .cmp(&right_file.uri)
                .then(
                    left.selection_range
                        .start
                        .line
                        .cmp(&right.selection_range.start.line),
                )
                .then(
                    left.selection_range
                        .start
                        .character
                        .cmp(&right.selection_range.start.character),
                )
        });
        candidates.dedup_by(|(left_file, left), (right_file, right)| {
            left_file.uri == right_file.uri && left.selection_range == right.selection_range
        });

        if candidates.len() == 1 {
            candidates.pop()
        } else {
            None
        }
    }

    /// Find the exact cached circuit represented by a client-round-tripped item.
    fn circuit_for_item<'a>(
        files: &'a [CallHierarchyFile],
        item: &CallHierarchyItem,
    ) -> Option<(&'a CallHierarchyFile, &'a CircuitDefinition)> {
        let item_uri = Self::cache_uri(&item.uri.to_string());
        let file = files.iter().find(|file| file.uri == item_uri)?;
        let circuit = file.document.circuits.iter().find(|circuit| {
            circuit.name == item.name && circuit.selection_range == item.selection_range
        })?;
        Some((file, circuit))
    }

    /// Convert one analyzer circuit into the protocol item preserved by clients.
    fn call_hierarchy_item(
        file: &CallHierarchyFile,
        circuit: &CircuitDefinition,
    ) -> Option<CallHierarchyItem> {
        Some(CallHierarchyItem {
            name: circuit.name.clone(),
            kind: SymbolKind::FUNCTION,
            tags: None,
            detail: Some("circuit".to_string()),
            uri: file.uri.parse::<Uri>().ok()?,
            range: circuit.range,
            selection_range: circuit.selection_range,
            data: None,
        })
    }

    /// Stable identity used to group duplicate call sites without collapsing
    /// distinct declarations that happen to share a name.
    fn same_call_hierarchy_item(left: &CallHierarchyItem, right: &CallHierarchyItem) -> bool {
        left.uri == right.uri && left.selection_range == right.selection_range
    }

    /// Convert an inner-to-outer analyzer chain to the recursive LSP shape.
    ///
    /// Building from the outside inward guarantees that every `parent` contains
    /// the range that precedes it in the analyzer output.
    fn selection_range_chain(ranges: &[Range]) -> SelectionRange {
        let mut parent = None;
        for range in ranges.iter().rev() {
            parent = Some(Box::new(SelectionRange {
                range: *range,
                parent,
            }));
        }
        parent.map(|range| *range).unwrap_or_default()
    }

    /// Build a preferred insertion quick fix for one trusted missing-token diagnostic.
    ///
    /// Only parser diagnostics with the exact `compact-syntax` source and message
    /// prefix are considered. The token must also pass [`Self::safe_missing_token`];
    /// every other diagnostic returns `None` instead of turning arbitrary compiler
    /// text into an editor-applied source edit.
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

    /// Map the parser's missing-token spelling to the punctuation safe to insert.
    ///
    /// This explicit allowlist excludes identifiers, keywords, and structured
    /// syntax whose correct text or location cannot be inferred from one
    /// diagnostic. Unknown spellings return `None`.
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

    /// Return whether an LSP code-action request includes quick fixes.
    ///
    /// An absent `only` filter requests every supported action kind. When the
    /// filter is present, this first implementation accepts only the exact
    /// `quickfix` kind because it does not yet advertise child quick-fix kinds.
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
            next_diagnostic_generation: Arc::new(AtomicU64::new(1)),
            reverse_dependencies: Arc::new(DashMap::new()),
        }
    }

    /// Cancel and remove a document's pending semantic-diagnostic task.
    ///
    /// Aborting the Tokio task drops the compiler child configured with
    /// `kill_on_drop`, so superseded compiler processes do not continue running.
    fn cancel_pending_diagnostics(&self, uri: &str) {
        if let Some((_, task)) = self.pending_diagnostics.remove(uri) {
            task.handle.abort();
        }
    }

    /// Cancel every pending diagnostic task during server shutdown.
    fn cancel_all_pending_diagnostics(&self) {
        let uris: Vec<_> = self
            .pending_diagnostics
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        for uri in uris {
            self.cancel_pending_diagnostics(&uri);
        }
    }

    /// Return whether a document still has the version captured by a request.
    fn document_version_matches(
        documents: &DashMap<String, Document>,
        uri: &str,
        expected_version: i32,
    ) -> bool {
        documents
            .get(uri)
            .map(|document| document.version == expected_version)
            .unwrap_or(false)
    }

    /// Verify both the document version and the task generation before publication.
    ///
    /// Checking the generation closes the race where an older task finishes just
    /// as a newer task is inserted for the same document.
    fn diagnostic_task_is_current(
        pending: &DashMap<String, PendingDiagnosticTask>,
        documents: &DashMap<String, Document>,
        uri: &str,
        generation: u64,
        expected_version: i32,
    ) -> bool {
        pending
            .get(uri)
            .map(|task| task.generation == generation)
            .unwrap_or(false)
            && Self::document_version_matches(documents, uri, expected_version)
    }

    /// Remove task bookkeeping only when it still belongs to this generation.
    fn clear_pending_diagnostic_if_current(
        pending: &DashMap<String, PendingDiagnosticTask>,
        uri: &str,
        generation: u64,
    ) {
        pending.remove_if(uri, |_, task| task.generation == generation);
    }

    /// Publish diagnostics for a closed workspace file or schedule them for an open one.
    ///
    /// Open documents always use the tracked, cancellable task path, even on open
    /// and save. Closed workspace files cannot receive edits, so they are compiled
    /// directly from disk without an LSP version.
    async fn publish_diagnostics(&self, uri: Uri) {
        let uri_string = uri.to_string();
        if let Some(document) = self.documents.get(&uri_string) {
            let content = document.content.to_string();
            let version = document.version;
            drop(document);
            self.publish_syntax_diagnostics(uri.clone()).await;
            self.schedule_semantic_diagnostics(uri, content, version, Duration::ZERO)
                .await;
            return;
        }

        let Some(path) = imports::file_uri_to_path(&uri_string) else {
            return;
        };
        let content = match tokio::fs::read_to_string(path).await {
            Ok(content) => content,
            Err(_) => return,
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

    /// Publish versioned syntax diagnostics for the latest open-document snapshot.
    async fn publish_syntax_diagnostics(&self, uri: Uri) {
        let uri_string = uri.to_string();
        let (content, version) = match self.documents.get(&uri_string) {
            Some(doc) => (doc.content.to_string(), doc.version),
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

        if !Self::document_version_matches(&self.documents, &uri_string, version) {
            return;
        }
        self.client
            .publish_diagnostics(uri, diagnostics, Some(version))
            .await;
    }

    /// Schedule versioned semantic diagnostics after a short debounce.
    ///
    /// Replacement aborts the previous task and compiler child. The task checks
    /// both its generation and document version before and after compilation, then
    /// removes only its own bookkeeping entry.
    async fn schedule_semantic_diagnostics(
        &self,
        uri: Uri,
        content: String,
        version: i32,
        debounce: Duration,
    ) {
        let uri_string = uri.to_string();
        self.cancel_pending_diagnostics(&uri_string);
        let generation = self
            .next_diagnostic_generation
            .fetch_add(1, Ordering::AcqRel);

        let client = self.client.clone();
        let diagnostic_engine = self.diagnostic_engine.clone();
        let parser_engine = self.parser_engine.clone();
        let pending = self.pending_diagnostics.clone();
        let documents = self.documents.clone();
        let uri_clone = uri_string.clone();
        // A zero-debounce task can run on another worker immediately. Gate it
        // until its generation is visible in the pending-task map.
        let (start_sender, start_receiver) = tokio::sync::oneshot::channel();

        let handle = tokio::spawn(async move {
            if start_receiver.await.is_err() {
                return;
            }
            if !debounce.is_zero() {
                tokio::time::sleep(debounce).await;
            }
            if !Self::diagnostic_task_is_current(
                &pending, &documents, &uri_clone, generation, version,
            ) {
                Self::clear_pending_diagnostic_if_current(&pending, &uri_clone, generation);
                return;
            }

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

            if Self::diagnostic_task_is_current(
                &pending, &documents, &uri_clone, generation, version,
            ) {
                client
                    .publish_diagnostics(uri, all_diagnostics, Some(version))
                    .await;
            } else {
                tracing::debug!(
                    "Discarding stale semantic diagnostics for {} at version {}",
                    uri_clone,
                    version
                );
            }
            Self::clear_pending_diagnostic_if_current(&pending, &uri_clone, generation);
        });

        self.pending_diagnostics
            .insert(uri_string, PendingDiagnosticTask { generation, handle });
        let _ = start_sender.send(());
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

    /// Read and parse every Compact file below the supplied workspace roots.
    ///
    /// This synchronous function runs inside `spawn_blocking`. Canonical URI
    /// deduplication prevents nested or overlapping workspace roots from indexing
    /// a file twice, and unreadable files are skipped without failing the scan.
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

                let source_index = parser.index_source(&content);
                indexed.push(IndexedWorkspaceFile {
                    uri,
                    content,
                    symbols: source_index.symbols,
                    imports: source_index.imports,
                });
            }
        }

        indexed
    }

    /// Replace the logical workspace snapshot after a completed scan.
    ///
    /// Open editor buffers are reapplied last because their unsaved text is newer
    /// than the corresponding disk snapshot. The enclosing scan lock prevents two
    /// full replacements from interleaving.
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
            self.update_file_cache(&uri, &content);
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

    /// Return the canonical cache key for a file URI when its path is resolvable.
    ///
    /// New files may not exist yet, so the parent directory is canonicalized as a
    /// fallback before the lexical normalization used by import resolution.
    fn normalized_file_uri(uri: &str) -> Option<String> {
        let path = imports::file_uri_to_path(uri)?;
        let normalized = path.canonicalize().ok().or_else(|| {
            let parent = path.parent()?.canonicalize().ok()?;
            Some(parent.join(path.file_name()?))
        });
        let normalized = normalized.or_else(|| imports::normalize_path(&path))?;
        imports::path_to_file_uri(&normalized)
    }

    /// Normalize a file URI for cache access, preserving non-file or unresolved URIs.
    fn cache_uri(uri: &str) -> String {
        Self::normalized_file_uri(uri).unwrap_or_else(|| uri.to_string())
    }

    /// Check whether a URI names a Compact file under any configured workspace root.
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

    /// Refresh one workspace file from its open buffer or, otherwise, from disk.
    ///
    /// Returns the normalized cache URI only when the file remains readable and in
    /// scope. Callers use `None` to remove stale entries after deletes or renames.
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

        self.update_file_cache(&cache_uri, &content);
        Some(cache_uri)
    }

    /// Remove all cached state for a file and return its former dependents.
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

    /// Rebuild all parser-derived cache entries for one source snapshot.
    ///
    /// Symbols and imports come from the same syntax tree, keeping the caches
    /// internally consistent while avoiding duplicate parsing. Empty symbol sets
    /// remove old declarations, and dependency edges are always replaced.
    fn update_file_cache(&self, uri: &str, content: &str) {
        let uri = Self::cache_uri(uri);
        let source_index = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser.index_source(content)
        };

        self.source_cache.insert(uri.clone(), content.to_string());

        if source_index.symbols.is_empty() {
            self.symbol_cache.remove(&uri);
        } else {
            self.symbol_cache.insert(uri.clone(), source_index.symbols);
        }

        self.remove_reverse_dependencies(&uri);
        self.add_reverse_dependencies(&uri, &source_index.imports);
    }

    /// Build a deterministic `workspace/symbol` response from a symbol-cache snapshot.
    ///
    /// Taking owned entries ensures no `DashMap` guard is held while the results are
    /// filtered and sorted. Identical declarations are deduplicated by kind, URI,
    /// name, and UTF-16 range, while distinct declarations with the same name remain.
    fn workspace_symbol_results(
        entries: Vec<(String, Vec<CompletionSymbol>)>,
        query: &str,
    ) -> Vec<SymbolInformation> {
        let normalized_query = query.trim().to_lowercase();
        let mut candidates = Vec::new();

        for (uri_string, symbols) in entries {
            let Ok(uri) = uri_string.parse::<Uri>() else {
                continue;
            };
            let container_name = imports::file_uri_to_path(&uri_string).and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            });

            for symbol in symbols {
                let Some(location) = symbol.location else {
                    continue;
                };
                let normalized_name = symbol.name.to_lowercase();
                let Some(match_rank) =
                    Self::workspace_symbol_match_rank(&normalized_name, &normalized_query)
                else {
                    continue;
                };
                let (kind, kind_rank) = Self::workspace_symbol_kind(symbol.kind);

                #[allow(deprecated)]
                let information = SymbolInformation {
                    name: symbol.name,
                    kind,
                    tags: None,
                    deprecated: None,
                    location: Location {
                        uri: uri.clone(),
                        range: Range {
                            start: Position {
                                line: location.start_line,
                                character: location.start_char,
                            },
                            end: Position {
                                line: location.end_line,
                                character: location.end_char,
                            },
                        },
                    },
                    container_name: container_name.clone(),
                };

                candidates.push(WorkspaceSymbolCandidate {
                    match_rank,
                    normalized_name,
                    kind_rank,
                    uri: uri_string.clone(),
                    information,
                });
            }
        }

        candidates.sort_by(|left, right| {
            left.match_rank
                .cmp(&right.match_rank)
                .then(left.normalized_name.cmp(&right.normalized_name))
                .then(left.information.name.cmp(&right.information.name))
                .then(left.kind_rank.cmp(&right.kind_rank))
                .then(left.uri.cmp(&right.uri))
                .then(
                    left.information
                        .location
                        .range
                        .start
                        .line
                        .cmp(&right.information.location.range.start.line),
                )
                .then(
                    left.information
                        .location
                        .range
                        .start
                        .character
                        .cmp(&right.information.location.range.start.character),
                )
                .then(
                    left.information
                        .location
                        .range
                        .end
                        .line
                        .cmp(&right.information.location.range.end.line),
                )
                .then(
                    left.information
                        .location
                        .range
                        .end
                        .character
                        .cmp(&right.information.location.range.end.character),
                )
        });
        candidates.dedup_by(|left, right| {
            left.kind_rank == right.kind_rank
                && left.uri == right.uri
                && left.information.name == right.information.name
                && left.information.location.range == right.information.location.range
        });

        candidates
            .into_iter()
            .map(|candidate| candidate.information)
            .collect()
    }

    /// Rank a normalized symbol name against a normalized query.
    ///
    /// Lower values sort first. Empty queries intentionally include every cached
    /// declaration, while non-matches are excluded.
    fn workspace_symbol_match_rank(name: &str, query: &str) -> Option<u8> {
        if query.is_empty() {
            Some(3)
        } else if name == query {
            Some(0)
        } else if name.starts_with(query) {
            Some(1)
        } else if name.contains(query) {
            Some(2)
        } else {
            None
        }
    }

    /// Convert an analyzer declaration kind to its LSP kind and stable sort rank.
    fn workspace_symbol_kind(kind: compact_analyzer::CompletionSymbolKind) -> (SymbolKind, u8) {
        match kind {
            compact_analyzer::CompletionSymbolKind::Function => (SymbolKind::FUNCTION, 0),
            compact_analyzer::CompletionSymbolKind::Struct => (SymbolKind::STRUCT, 1),
            compact_analyzer::CompletionSymbolKind::Enum => (SymbolKind::ENUM, 2),
            compact_analyzer::CompletionSymbolKind::Variable => (SymbolKind::VARIABLE, 3),
            compact_analyzer::CompletionSymbolKind::Module => (SymbolKind::MODULE, 4),
        }
    }

    /// Resolve an exact call to parameter names without guessing between overloads.
    ///
    /// Local declarations take precedence over imports and the standard library. If
    /// multiple local declarations have the same name, no hints are returned because
    /// the tree-sitter analyzer cannot yet prove which declaration the compiler chose.
    /// Member calls resolve through the receiver's ledger type, including `kernel`.
    fn inlay_parameter_names(
        &self,
        current_uri: &str,
        call: &compact_analyzer::CallSite,
        index: &InlaySignatureIndex,
        imports: &[ImportInfo],
    ) -> Option<Vec<String>> {
        let detail = if let Some(receiver) = &call.receiver {
            let receiver_type = index
                .ledger_types
                .get(receiver)
                .map(String::as_str)
                .or_else(|| (receiver == "kernel").then_some("Kernel"))?;
            let base_type = builtins::extract_base_type(receiver_type);
            builtins::find_method_by_name(base_type, &call.function_name)?
                .signature
                .to_string()
        } else {
            match index.local_functions.get(&call.function_name) {
                Some(Some(detail)) => detail.clone(),
                Some(None) => return None,
                None => {
                    if let Some((_uri, symbol)) = self.find_imported_symbol_from_imports(
                        current_uri,
                        &call.function_name,
                        imports,
                    ) {
                        symbol.detail?
                    } else {
                        stdlib::find_stdlib_circuit(&call.function_name)?
                            .signature
                            .to_string()
                    }
                }
            }
        };

        Self::parameter_names_from_detail(&detail)
    }

    /// Build the reusable local signature and ledger-type snapshot for one request.
    ///
    /// Duplicate function declarations are recorded as ambiguous even if their
    /// display signatures happen to match, because source order is not resolution.
    fn inlay_signature_index(symbols: Vec<CompletionSymbol>) -> InlaySignatureIndex {
        let mut local_functions = HashMap::new();
        let mut ledger_types = HashMap::new();

        for symbol in symbols {
            match symbol.kind {
                compact_analyzer::CompletionSymbolKind::Function => {
                    local_functions
                        .entry(symbol.name)
                        .and_modify(|detail| *detail = None)
                        .or_insert(symbol.detail);
                }
                compact_analyzer::CompletionSymbolKind::Variable => {
                    if let Some(receiver_type) = symbol
                        .detail
                        .as_deref()
                        .and_then(|detail| detail.strip_prefix("ledger: "))
                    {
                        ledger_types.insert(symbol.name, receiver_type.to_string());
                    }
                }
                _ => {}
            }
        }

        InlaySignatureIndex {
            local_functions,
            ledger_types,
        }
    }

    /// Extract simple parameter identifiers from a display signature.
    ///
    /// Complex parameter patterns are intentionally rejected: displaying a partial
    /// pattern as a name would turn a helpful hint into misleading source text.
    fn parameter_names_from_detail(detail: &str) -> Option<Vec<String>> {
        utils::parse_params_from_detail(detail)
            .into_iter()
            .map(|label| {
                let (name, _) = label.split_once(':')?;
                let name = name.trim();
                let mut characters = name.chars();
                let first = characters.next()?;
                if !(first == '_' || first.is_ascii_alphabetic())
                    || !characters
                        .all(|character| character == '_' || character.is_ascii_alphanumeric())
                {
                    return None;
                }
                Some(name.to_string())
            })
            .collect()
    }

    /// Test whether a UTF-16 position is inside an LSP range.
    ///
    /// LSP ranges are start-inclusive and end-exclusive. Applying the requested
    /// viewport range here avoids returning hints the editor will not render.
    fn position_in_range(position: Position, range: Range) -> bool {
        let key = |position: Position| (position.line, position.character);
        key(position) >= key(range.start) && key(position) < key(range.end)
    }

    /// Add reverse edges from imported files to the file that depends on them.
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

        self.find_imported_symbol_from_imports(current_uri, name, &file_imports)
    }

    /// Resolve a prefixed symbol against an already parsed import snapshot.
    ///
    /// Keeping this lookup separate lets bulk features such as inlay hints parse
    /// imports once, while point queries can continue using `find_imported_symbol`.
    fn find_imported_symbol_from_imports(
        &self,
        current_uri: &str,
        name: &str,
        file_imports: &[ImportInfo],
    ) -> Option<(String, CompletionSymbol)> {
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

    /// Populate missing imported-symbol entries before a bulk editor request resolves them.
    ///
    /// Workspace indexing runs in the background, so an editor can request hints
    /// immediately after opening a document. Reading only cache misses here removes
    /// that startup race. Missing or unreadable imports remain unresolved and produce
    /// no hint; they are reported through the normal diagnostic path.
    async fn cache_missing_imports(&self, current_uri: &str, file_imports: &[ImportInfo]) {
        for import in file_imports {
            if !import.is_file {
                continue;
            }
            let Some(imported_uri) = imports::resolve_import_path(current_uri, &import.path) else {
                continue;
            };
            if self.symbol_cache.contains_key(&imported_uri) {
                continue;
            }
            let Some(path) = imports::file_uri_to_path(&imported_uri) else {
                continue;
            };
            match tokio::fs::read_to_string(path).await {
                Ok(content) => self.update_file_cache(&imported_uri, &content),
                Err(error) => tracing::debug!(
                    "Could not load imported symbols for inlay hints from {}: {}",
                    imported_uri,
                    error
                ),
            }
        }
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
                        change: Some(TextDocumentSyncKind::INCREMENTAL),
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
                selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
                workspace_symbol_provider: Some(OneOf::Right(WorkspaceSymbolOptions {
                    resolve_provider: Some(false),
                    work_done_progress_options: Default::default(),
                })),
                document_highlight_provider: Some(OneOf::Left(true)),
                inlay_hint_provider: Some(OneOf::Right(InlayHintServerCapabilities::Options(
                    InlayHintOptions {
                        resolve_provider: Some(false),
                        work_done_progress_options: Default::default(),
                    },
                ))),
                linked_editing_range_provider: Some(LinkedEditingRangeServerCapabilities::Simple(
                    true,
                )),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
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
        self.cancel_all_pending_diagnostics();
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
        self.cancel_pending_diagnostics(&uri);

        let rope = Rope::from_str(&params.text_document.text);
        self.documents.insert(
            uri.clone(),
            Document {
                content: rope,
                version: params.text_document.version,
            },
        );

        self.update_file_cache(&uri, &params.text_document.text);
        self.publish_diagnostics(params.text_document.uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let uri_string = uri.to_string();
        let version = params.text_document.version;

        let updated = match self.documents.get_mut(&uri_string) {
            Some(mut document) => {
                match document.apply_changes_if_newer(version, &params.content_changes) {
                    Ok(updated) => updated,
                    Err(error) => {
                        tracing::warn!("Ignoring invalid change for {}: {}", uri_string, error);
                        return;
                    }
                }
            }
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

        self.cancel_pending_diagnostics(&uri_string);
        self.publish_syntax_diagnostics(uri.clone()).await;

        let content = match self.documents.get(&uri_string) {
            Some(doc) => doc.content.to_string(),
            None => return,
        };
        self.update_file_cache(&uri_string, &content);
        self.schedule_semantic_diagnostics(uri, content, version, Duration::from_millis(500))
            .await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let uri_str = uri.to_string();
        self.cancel_pending_diagnostics(&uri_str);

        if let Some(doc) = self.documents.get(&uri_str) {
            let content = doc.content.to_string();
            self.update_file_cache(&uri_str, &content);
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
        self.cancel_pending_diagnostics(&uri);

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

            let source_index = {
                let mut parser = self.parser_engine.lock().unwrap();
                parser.index_source(&content)
            };

            for sym in source_index.symbols {
                items.push(CompletionItem {
                    label: sym.name.clone(),
                    kind: Some(symbol_to_lsp_kind(sym.kind)),
                    detail: sym.detail,
                    insert_text: Some(sym.name),
                    ..Default::default()
                });
            }

            source_index.imports
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

    /// Convert eligible request diagnostics into conservative punctuation quick fixes.
    ///
    /// Diagnostics are evaluated independently and unsafe entries are omitted.
    /// The handler returns an empty response for unsupported action-kind filters
    /// rather than executing edits or guessing at compiler intent.
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

    /// Expand every requested position through named Compact syntax ancestors.
    async fn selection_range(
        &self,
        params: SelectionRangeParams,
    ) -> Result<Option<Vec<SelectionRange>>> {
        let uri = params.text_document.uri.to_string();
        let content = match self.documents.get(&uri) {
            Some(document) => document.content.to_string(),
            None => return Ok(None),
        };
        let chains = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser.selection_range_chains(&content, &params.positions)
        };

        Ok(Some(
            chains
                .iter()
                .map(|ranges| Self::selection_range_chain(ranges))
                .collect(),
        ))
    }

    /// Search the current open-document and indexed-workspace symbol cache.
    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<WorkspaceSymbolResponse>> {
        let entries = self
            .symbol_cache
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();
        let symbols = Self::workspace_symbol_results(entries, &params.query);

        Ok(Some(WorkspaceSymbolResponse::Flat(symbols)))
    }

    /// Highlight resolved declarations and references inside the requested document.
    ///
    /// Parser definitions are reported as writes and usages as reads. A usage-only
    /// result is returned only when the symbol resolves through an import, preventing
    /// unresolved identifiers from being highlighted merely because their text matches.
    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .to_string();
        let position = params.text_document_position_params.position;
        let content = match self.documents.get(&uri) {
            Some(document) => document.content.to_string(),
            None => return Ok(None),
        };
        let Some(symbol_name) =
            utils::get_word_at_position(&content, position.line, position.character)
        else {
            return Ok(None);
        };
        if validation::is_keyword(&symbol_name) || validation::is_builtin_type(&symbol_name) {
            return Ok(None);
        }

        let references = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser.find_references(&content, &symbol_name)
        };
        let is_resolved_locally = references.iter().any(|reference| reference.is_definition);
        let is_resolved_import = self.find_imported_symbol(&uri, &symbol_name).is_some();
        if !is_resolved_locally && !is_resolved_import {
            return Ok(None);
        }

        let mut highlights: Vec<_> = references
            .into_iter()
            .map(|reference| DocumentHighlight {
                range: reference.range,
                kind: Some(if reference.is_definition {
                    DocumentHighlightKind::WRITE
                } else {
                    DocumentHighlightKind::READ
                }),
            })
            .collect();
        highlights.sort_by_key(|highlight| {
            (
                highlight.range.start.line,
                highlight.range.start.character,
                highlight.range.end.line,
                highlight.range.end.character,
            )
        });
        highlights.dedup_by_key(|highlight| highlight.range);

        if highlights.is_empty() {
            Ok(None)
        } else {
            Ok(Some(highlights))
        }
    }

    /// Link a uniquely declared import prefix to its direct prefixed calls.
    ///
    /// The parser enforces the LSP requirement that every range contains the
    /// same text and rejects syntax-only ambiguities. Returning `None` is
    /// intentional for unsupported or unresolved constructs; it prevents an
    /// editor from mirroring an unsafe partial rename while the user types.
    async fn linked_editing_range(
        &self,
        params: LinkedEditingRangeParams,
    ) -> Result<Option<LinkedEditingRanges>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .to_string();
        let position = params.text_document_position_params.position;
        let content = match self.documents.get(&uri) {
            Some(document) => document.content.to_string(),
            None => return Ok(None),
        };

        let ranges = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser.linked_import_prefix_ranges(&content, position.line, position.character)
        };

        Ok(ranges.map(|ranges| LinkedEditingRanges {
            ranges,
            word_pattern: Some("[A-Za-z_][A-Za-z0-9_]*".to_string()),
        }))
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

    /// Return conservative parameter-name hints for complete calls in the requested range.
    ///
    /// A call must resolve to one known signature with exactly the observed arity.
    /// Hints are suppressed for incomplete or ambiguous calls and when an argument
    /// already has the same identifier as its parameter. Type hints are deferred
    /// until compiler-backed inference is available.
    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri.to_string();
        let content = match self.documents.get(&uri) {
            Some(document) => document.content.to_string(),
            None => return Ok(None),
        };
        let calls = {
            let mut parser = self.parser_engine.lock().unwrap();
            parser.call_sites(&content)
        };
        let (index, imports) = {
            let mut parser = self.parser_engine.lock().unwrap();
            (
                Self::inlay_signature_index(parser.get_completion_symbols(&content)),
                parser.get_imports(&content),
            )
        };
        self.cache_missing_imports(&uri, &imports).await;
        let mut hints = Vec::new();

        for call in calls {
            let Some(parameter_names) = self.inlay_parameter_names(&uri, &call, &index, &imports)
            else {
                continue;
            };
            if parameter_names.len() != call.arguments.len() {
                continue;
            }

            for (argument, parameter_name) in call.arguments.iter().zip(parameter_names) {
                if argument.text.trim() == parameter_name
                    || !Self::position_in_range(argument.position, params.range)
                {
                    continue;
                }
                hints.push((
                    argument.position,
                    parameter_name.clone(),
                    InlayHint {
                        position: argument.position,
                        label: format!("{parameter_name}:").into(),
                        kind: Some(InlayHintKind::PARAMETER),
                        text_edits: None,
                        tooltip: Some(format!("Parameter `{parameter_name}`").into()),
                        padding_left: None,
                        padding_right: Some(true),
                        data: None,
                    },
                ));
            }
        }

        hints.sort_by(|left, right| {
            (left.0.line, left.0.character, left.1.as_str()).cmp(&(
                right.0.line,
                right.0.character,
                right.1.as_str(),
            ))
        });
        hints.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
        Ok(Some(hints.into_iter().map(|(_, _, hint)| hint).collect()))
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

    /// Prepare exactly one circuit item from a declaration or resolved direct call.
    async fn prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> Result<Option<Vec<CallHierarchyItem>>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .to_string();
        let position = params.text_document_position_params.position;
        let cache_uri = Self::cache_uri(&uri);
        let files = self.call_hierarchy_files();
        let Some(file) = files.iter().find(|file| file.uri == cache_uri) else {
            return Ok(None);
        };

        let mut candidates = Vec::new();
        for circuit in &file.document.circuits {
            if Self::position_in_range(position, circuit.selection_range) {
                if let Some(item) = Self::call_hierarchy_item(file, circuit) {
                    candidates.push(item);
                }
            }
            for call in &circuit.calls {
                if !Self::position_in_range(position, call.range) {
                    continue;
                }
                if let Some((target_file, target)) =
                    Self::resolve_call_target(&files, file, &call.name)
                {
                    if let Some(item) = Self::call_hierarchy_item(target_file, target) {
                        candidates.push(item);
                    }
                }
            }
        }

        candidates.sort_by(|left, right| {
            left.uri
                .as_str()
                .cmp(right.uri.as_str())
                .then(left.name.cmp(&right.name))
                .then(
                    left.selection_range
                        .start
                        .line
                        .cmp(&right.selection_range.start.line),
                )
                .then(
                    left.selection_range
                        .start
                        .character
                        .cmp(&right.selection_range.start.character),
                )
        });
        candidates.dedup_by(|left, right| Self::same_call_hierarchy_item(left, right));

        if candidates.len() == 1 {
            Ok(Some(candidates))
        } else {
            Ok(None)
        }
    }

    /// Group every unambiguous workspace call to the selected circuit by caller.
    async fn incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyIncomingCall>>> {
        let files = self.call_hierarchy_files();
        let Some((target_file, target)) = Self::circuit_for_item(&files, &params.item) else {
            return Ok(None);
        };
        let Some(target_item) = Self::call_hierarchy_item(target_file, target) else {
            return Ok(None);
        };

        let mut incoming = Vec::<CallHierarchyIncomingCall>::new();
        for caller_file in &files {
            for caller in &caller_file.document.circuits {
                let Some(caller_item) = Self::call_hierarchy_item(caller_file, caller) else {
                    continue;
                };
                let mut from_ranges = Vec::new();
                for call in &caller.calls {
                    let Some((called_file, called)) =
                        Self::resolve_call_target(&files, caller_file, &call.name)
                    else {
                        continue;
                    };
                    let Some(called_item) = Self::call_hierarchy_item(called_file, called) else {
                        continue;
                    };
                    if Self::same_call_hierarchy_item(&called_item, &target_item) {
                        from_ranges.push(call.range);
                    }
                }
                if !from_ranges.is_empty() {
                    from_ranges.sort_by_key(|range| (range.start.line, range.start.character));
                    incoming.push(CallHierarchyIncomingCall {
                        from: caller_item,
                        from_ranges,
                    });
                }
            }
        }

        incoming.sort_by(|left, right| {
            left.from
                .uri
                .as_str()
                .cmp(right.from.uri.as_str())
                .then(left.from.name.cmp(&right.from.name))
                .then(
                    left.from
                        .selection_range
                        .start
                        .line
                        .cmp(&right.from.selection_range.start.line),
                )
                .then(
                    left.from
                        .selection_range
                        .start
                        .character
                        .cmp(&right.from.selection_range.start.character),
                )
        });

        Ok(Some(incoming))
    }

    /// Group the selected circuit's unambiguous direct calls by target.
    async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        let files = self.call_hierarchy_files();
        let Some((caller_file, caller)) = Self::circuit_for_item(&files, &params.item) else {
            return Ok(None);
        };

        let mut outgoing = Vec::<CallHierarchyOutgoingCall>::new();
        for call in &caller.calls {
            let Some((target_file, target)) =
                Self::resolve_call_target(&files, caller_file, &call.name)
            else {
                continue;
            };
            let Some(target_item) = Self::call_hierarchy_item(target_file, target) else {
                continue;
            };

            if let Some(existing) = outgoing
                .iter_mut()
                .find(|existing| Self::same_call_hierarchy_item(&existing.to, &target_item))
            {
                existing.from_ranges.push(call.range);
            } else {
                outgoing.push(CallHierarchyOutgoingCall {
                    to: target_item,
                    from_ranges: vec![call.range],
                });
            }
        }

        for call in &mut outgoing {
            call.from_ranges
                .sort_by_key(|range| (range.start.line, range.start.character));
        }
        outgoing.sort_by(|left, right| {
            left.to
                .uri
                .as_str()
                .cmp(right.to.uri.as_str())
                .then(left.to.name.cmp(&right.to.name))
                .then(
                    left.to
                        .selection_range
                        .start
                        .line
                        .cmp(&right.to.selection_range.start.line),
                )
                .then(
                    left.to
                        .selection_range
                        .start
                        .character
                        .cmp(&right.to.selection_range.start.character),
                )
        });

        Ok(Some(outgoing))
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

    fn cached_symbol(
        name: &str,
        kind: compact_analyzer::CompletionSymbolKind,
        character: u32,
    ) -> CompletionSymbol {
        CompletionSymbol {
            name: name.to_string(),
            kind,
            detail: None,
            location: Some(compact_analyzer::SymbolLocation {
                start_line: 1,
                start_char: character,
                end_line: 1,
                end_char: character + name.encode_utf16().count() as u32,
            }),
            documentation: None,
        }
    }

    fn call_hierarchy_file(path: &std::path::Path, source: &str) -> CallHierarchyFile {
        std::fs::write(path, source).unwrap();
        let mut parser = ParserEngine::new();
        CallHierarchyFile {
            uri: imports::path_to_file_uri(&path.canonicalize().unwrap()).unwrap(),
            document: parser.call_hierarchy(source),
        }
    }

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

    #[test]
    fn workspace_symbols_are_ranked_sorted_and_deduplicated() {
        let alpha = cached_symbol("alpha", compact_analyzer::CompletionSymbolKind::Function, 4);
        let entries = vec![
            (
                "file:///workspace/B.compact".to_string(),
                vec![cached_symbol(
                    "contains_alpha",
                    compact_analyzer::CompletionSymbolKind::Variable,
                    8,
                )],
            ),
            (
                "file:///workspace/A.compact".to_string(),
                vec![
                    cached_symbol(
                        "alphabet",
                        compact_analyzer::CompletionSymbolKind::Struct,
                        2,
                    ),
                    alpha.clone(),
                    alpha,
                ],
            ),
        ];

        let symbols = CompactLanguageServer::workspace_symbol_results(entries, "ALPHA");
        assert_eq!(
            symbols
                .iter()
                .map(|symbol| symbol.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "alphabet", "contains_alpha"]
        );
        assert_eq!(symbols[0].location.range.start.character, 4);
        assert_eq!(symbols[0].container_name.as_deref(), Some("A.compact"));
        assert_eq!(symbols[0].kind, SymbolKind::FUNCTION);
    }

    #[test]
    fn inlay_parameter_names_accept_only_simple_identifiers() {
        assert_eq!(
            CompactLanguageServer::parameter_names_from_detail(
                "send(input: QualifiedCoinInfo, recipient: Either<A, B>): SendResult"
            ),
            Some(vec!["input".to_string(), "recipient".to_string()])
        );
        assert_eq!(
            CompactLanguageServer::parameter_names_from_detail("(#size: Field): Field"),
            None
        );
    }

    #[test]
    fn inlay_ranges_are_start_inclusive_and_end_exclusive() {
        let range = Range {
            start: Position {
                line: 2,
                character: 4,
            },
            end: Position {
                line: 3,
                character: 0,
            },
        };

        assert!(CompactLanguageServer::position_in_range(range.start, range));
        assert!(!CompactLanguageServer::position_in_range(range.end, range));
    }

    #[test]
    fn call_hierarchy_resolves_prefixes_and_rejects_ambiguous_imports() {
        let temporary = tempfile::tempdir().unwrap();
        let main_path = temporary.path().join("Main.compact");
        let first_path = temporary.path().join("First.compact");
        let second_path = temporary.path().join("Second.compact");
        let declaration = "circuit shared(): Field { return 1; }";
        let first = call_hierarchy_file(&first_path, declaration);
        let second = call_hierarchy_file(&second_path, declaration);
        let prefixed = call_hierarchy_file(
            &main_path,
            "import \"./First\" prefix First_;\nimport \"./Second\" prefix Second_;\ncircuit caller(): Field { return First_shared(); }",
        );
        let files = vec![prefixed, first, second];

        let (target_file, target) =
            CompactLanguageServer::resolve_call_target(&files, &files[0], "First_shared")
                .expect("a unique prefix should resolve");
        assert_eq!(target_file.uri, files[1].uri);
        assert_eq!(target.name, "shared");

        let ambiguous = call_hierarchy_file(
            &main_path,
            "import \"./First\";\nimport \"./Second\";\ncircuit caller(): Field { return shared(); }",
        );
        let first = call_hierarchy_file(&first_path, declaration);
        let second = call_hierarchy_file(&second_path, declaration);
        let files = vec![ambiguous, first, second];

        assert!(
            CompactLanguageServer::resolve_call_target(&files, &files[0], "shared").is_none(),
            "two unprefixed imports must not become an arbitrary hierarchy edge"
        );
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
