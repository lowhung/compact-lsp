// This file is part of compact-lsp.
// Copyright (C) 2025 Midnight Foundation
// SPDX-License-Identifier: Apache-2.0

//! Formatter engine backed by the Compact formatter.

use crate::toolchain::{FormatterCommand, ToolSource};

/// Formatter engine that invokes either `compact format` or `format-compact`.
#[derive(Debug)]
pub struct FormatterEngine {
    formatter: Option<FormatterCommand>,
}

impl FormatterEngine {
    /// Create a formatter engine using automatic toolchain discovery.
    pub fn new() -> Self {
        let formatter = FormatterCommand::discover();

        if let Some(formatter) = &formatter {
            tracing::info!(
                "Found Compact formatter via {:?}: {}",
                formatter.source(),
                formatter.executable().display()
            );
        } else {
            tracing::warn!("Could not find the Compact CLI or format-compact");
        }

        Self { formatter }
    }

    /// Create an engine with a known formatter command.
    pub fn with_formatter(formatter: FormatterCommand) -> Self {
        Self {
            formatter: Some(formatter),
        }
    }

    /// Check whether a formatter command was discovered.
    pub fn is_available(&self) -> bool {
        self.formatter.is_some()
    }

    /// Format in-memory Compact source.
    pub async fn format(&self, content: &str) -> Result<String, String> {
        let Some(formatter) = &self.formatter else {
            return Err("formatter not available".to_string());
        };

        let temporary_source = tempfile::Builder::new()
            .prefix("compact-lsp-format-")
            .suffix(".compact")
            .tempfile()
            .map_err(|error| format!("failed to create temporary source: {error}"))?;

        tokio::fs::write(temporary_source.path(), content)
            .await
            .map_err(|error| format!("failed to write temporary source: {error}"))?;

        let args = formatter.format_arguments(temporary_source.path());
        tracing::debug!(
            "Running Compact formatter: {} {:?}",
            formatter.executable().display(),
            args
        );

        let output = formatter
            .command(&args)
            .output()
            .await
            .map_err(|error| format!("failed to run formatter: {error}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(if stderr.is_empty() {
                format!("formatter exited with {}", output.status)
            } else {
                format!("formatter failed: {stderr}")
            });
        }

        match formatter.source() {
            ToolSource::CompactCli => tokio::fs::read_to_string(temporary_source.path())
                .await
                .map_err(|error| format!("failed to read formatted source: {error}")),
            ToolSource::Direct => Ok(String::from_utf8_lossy(&output.stdout).to_string()),
        }
    }
}

impl Default for FormatterEngine {
    fn default() -> Self {
        Self::new()
    }
}
