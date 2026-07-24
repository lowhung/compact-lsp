// This file is part of compact-lsp.
// Copyright (C) 2025 Midnight Foundation
// SPDX-License-Identifier: Apache-2.0

//! Diagnostic engine backed by the Compact compiler.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};
use regex::Regex;
use semver::Version;

use crate::toolchain::{CompilerCommand, ToolSource};

/// Compatibility level between the detected compiler and this LSP release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilerCompatibility {
    /// The primary Compact 0.33 compatibility target.
    Primary,
    /// Compact 0.32 is supported where it shares the 0.33 implementation path.
    BestEffort,
    /// The compiler is outside the currently supported version range.
    Unsupported,
    /// The compiler did not return a semantic version.
    Unknown,
}

/// Compiler and language versions reported by the selected toolchain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerInfo {
    pub compiler_version: String,
    pub language_version: String,
    pub compatibility: CompilerCompatibility,
}

/// The diagnostic engine that wraps either the `compact` CLI or `compactc`.
#[derive(Debug)]
pub struct DiagnosticEngine {
    compiler: Option<CompilerCommand>,
}

impl DiagnosticEngine {
    /// Create a diagnostic engine using automatic toolchain discovery.
    pub fn new() -> Self {
        let compiler = CompilerCommand::discover();

        if let Some(compiler) = &compiler {
            tracing::info!(
                "Found Compact compiler via {:?}: {}",
                compiler.source(),
                compiler.executable().display()
            );
        } else {
            tracing::warn!("Could not find the Compact CLI or a compactc compiler");
        }

        Self { compiler }
    }

    /// Create an engine with a known compiler command.
    pub fn with_compiler(compiler: CompilerCommand) -> Self {
        Self {
            compiler: Some(compiler),
        }
    }

    /// Check whether a compiler command was discovered.
    pub fn is_available(&self) -> bool {
        self.compiler.is_some()
    }

    /// Describe how the compiler was discovered.
    pub fn source(&self) -> Option<ToolSource> {
        self.compiler.as_ref().map(CompilerCommand::source)
    }

    /// Query the selected compiler and language versions.
    pub async fn compiler_info(&self) -> Result<Option<CompilerInfo>, String> {
        let Some(compiler) = &self.compiler else {
            return Ok(None);
        };

        let compiler_version = self.query_info(compiler, "--version").await?;
        let language_version = self.query_info(compiler, "--language-version").await?;
        let compatibility = compiler_compatibility(&compiler_version);

        Ok(Some(CompilerInfo {
            compiler_version,
            language_version,
            compatibility,
        }))
    }

    /// Run diagnostics against the file on disk.
    pub async fn diagnose(&self, uri: &str, _content: &str) -> Vec<Diagnostic> {
        let Some(compiler) = &self.compiler else {
            tracing::warn!("Compiler not available, skipping diagnostics");
            return Vec::new();
        };

        let file_path = match file_uri_to_path(uri) {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!("Could not convert diagnostic URI to a file path: {}", error);
                return Vec::new();
            }
        };

        if !file_path.is_file() {
            tracing::warn!("File does not exist: {}", file_path.display());
            return Vec::new();
        }

        let output_dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => {
                tracing::error!("Failed to create compiler output directory: {}", error);
                return Vec::new();
            }
        };

        self.run_compiler(compiler, &file_path, output_dir.path())
            .await
    }

    /// Run diagnostics against in-memory content while keeping relative imports
    /// anchored to the original file's directory.
    pub async fn diagnose_content(&self, uri: &str, content: &str) -> Vec<Diagnostic> {
        let Some(compiler) = &self.compiler else {
            tracing::trace!("Compiler not available, skipping live diagnostics");
            return Vec::new();
        };

        let original_path = match file_uri_to_path(uri) {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!("Could not convert diagnostic URI to a file path: {}", error);
                return Vec::new();
            }
        };

        let Some(original_dir) = original_path.parent() else {
            tracing::warn!(
                "Could not determine the source directory for {}",
                original_path.display()
            );
            return Vec::new();
        };

        let temporary_source = match tempfile::Builder::new()
            .prefix(".compact-lsp-")
            .suffix(".compact")
            .tempfile_in(original_dir)
        {
            Ok(file) => file,
            Err(error) => {
                tracing::error!(
                    "Failed to create a temporary source beside {}: {}",
                    original_path.display(),
                    error
                );
                return Vec::new();
            }
        };

        if let Err(error) = tokio::fs::write(temporary_source.path(), content).await {
            tracing::error!("Failed to write temporary source: {}", error);
            return Vec::new();
        }

        let output_dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => {
                tracing::error!("Failed to create compiler output directory: {}", error);
                return Vec::new();
            }
        };

        self.run_compiler(compiler, temporary_source.path(), output_dir.path())
            .await
    }

    async fn query_info(&self, compiler: &CompilerCommand, flag: &str) -> Result<String, String> {
        let args = compiler.info_arguments(flag);
        let output = compiler
            .command(&args)
            .output()
            .await
            .map_err(|error| format!("failed to query compiler {flag}: {error}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(if stderr.is_empty() {
                format!("compiler {flag} exited with {}", output.status)
            } else {
                format!("compiler {flag} failed: {stderr}")
            });
        }

        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if version.is_empty() {
            Err(format!("compiler {flag} returned an empty version"))
        } else {
            Ok(version)
        }
    }

    async fn run_compiler(
        &self,
        compiler: &CompilerCommand,
        source: &Path,
        output_dir: &Path,
    ) -> Vec<Diagnostic> {
        let args = compiler.compile_arguments(source, output_dir);
        tracing::debug!(
            "Running Compact compiler: {} {:?}",
            compiler.executable().display(),
            args
        );

        let output = match compiler.command(&args).output().await {
            Ok(output) => output,
            Err(error) => {
                tracing::error!("Failed to run Compact compiler: {}", error);
                return Vec::new();
            }
        };

        tracing::debug!("Compiler exit status: {}", output.status);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        if !stderr.is_empty() {
            tracing::debug!("Compiler stderr: {}", stderr);
        }
        if !stdout.is_empty() {
            tracing::trace!("Compiler stdout: {}", stdout);
        }

        stderr
            .lines()
            .chain(stdout.lines())
            .filter_map(parse_error_line)
            .collect()
    }
}

impl Default for DiagnosticEngine {
    fn default() -> Self {
        Self::new()
    }
}

fn compiler_compatibility(version: &str) -> CompilerCompatibility {
    let Ok(version) = Version::parse(version.trim_start_matches('v')) else {
        return CompilerCompatibility::Unknown;
    };

    match (version.major, version.minor) {
        (0, 33) => CompilerCompatibility::Primary,
        (0, 32) => CompilerCompatibility::BestEffort,
        _ => CompilerCompatibility::Unsupported,
    }
}

fn file_uri_to_path(uri: &str) -> Result<PathBuf, String> {
    let uri = url::Url::parse(uri).map_err(|error| format!("invalid URI: {error}"))?;
    if uri.scheme() != "file" {
        return Err(format!("unsupported URI scheme: {}", uri.scheme()));
    }

    uri.to_file_path()
        .map_err(|_| "file URI cannot be represented as a local path".to_string())
}

/// Parse a single Compact compiler error line.
///
/// Format: `Exception: <filename> line <line> char <col>: <message>`.
fn parse_error_line(line: &str) -> Option<Diagnostic> {
    static ERROR_PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = ERROR_PATTERN.get_or_init(|| {
        Regex::new(r"^Exception:\s*(.+?)\s+line\s+(\d+)\s+char\s+(\d+):\s*(.+)$")
            .expect("compiler diagnostic regex must be valid")
    });

    let captures = pattern.captures(line)?;
    let line_num: u32 = captures.get(2)?.as_str().parse().ok()?;
    let column: u32 = captures.get(3)?.as_str().parse().ok()?;
    let message = captures.get(4)?.as_str().to_string();

    let line = line_num.saturating_sub(1);
    let character = column.saturating_sub(1);

    Some(Diagnostic {
        range: Range {
            start: Position { line, character },
            end: Position {
                line,
                character: character + 1,
            },
        },
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("compactc".to_string()),
        message,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_compiler_error() {
        let line = "Exception: broken.compact line 1 char 1: parse error: unexpected token";
        let diagnostic = parse_error_line(line).expect("expected compiler diagnostic");

        assert_eq!(diagnostic.range.start.line, 0);
        assert_eq!(diagnostic.range.start.character, 0);
        assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diagnostic.source.as_deref(), Some("compactc"));
        assert!(diagnostic.message.contains("parse error"));
    }

    #[test]
    fn parses_error_for_path_containing_spaces() {
        let line =
            "Exception: /tmp/Compact Project/broken name.compact line 4 char 3: type mismatch";
        let diagnostic = parse_error_line(line).expect("expected compiler diagnostic");

        assert_eq!(diagnostic.range.start.line, 3);
        assert_eq!(diagnostic.range.start.character, 2);
        assert_eq!(diagnostic.message, "type mismatch");
    }

    #[test]
    fn ignores_non_diagnostic_output() {
        assert!(parse_error_line("This is not an error line").is_none());
    }

    #[test]
    fn decodes_file_uri_paths() {
        let path = file_uri_to_path("file:///tmp/Compact%20Project/example.compact").unwrap();
        assert_eq!(path, PathBuf::from("/tmp/Compact Project/example.compact"));
    }

    #[test]
    fn rejects_non_file_uris() {
        let error = file_uri_to_path("untitled:example.compact").unwrap_err();
        assert!(error.contains("unsupported URI scheme"));
    }

    #[test]
    fn classifies_compiler_versions() {
        assert_eq!(
            compiler_compatibility("0.33.0-rc.2"),
            CompilerCompatibility::Primary
        );
        assert_eq!(
            compiler_compatibility("0.32.111"),
            CompilerCompatibility::BestEffort
        );
        assert_eq!(
            compiler_compatibility("0.31.1"),
            CompilerCompatibility::Unsupported
        );
        assert_eq!(
            compiler_compatibility("development"),
            CompilerCompatibility::Unknown
        );
    }
}
