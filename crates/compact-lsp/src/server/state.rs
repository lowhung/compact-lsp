//! Document state management.

use std::fmt;

use lsp_types::{Position, TextDocumentContentChangeEvent};
use ropey::Rope;

/// A document we're tracking (an open file in the editor).
#[derive(Debug, Clone)]
pub struct Document {
    /// The document content, stored as a rope for efficient editing.
    /// Rope is a data structure optimized for text editing:
    /// - O(log N) for insert/delete at any position
    /// - Cheap clones (structural sharing)
    pub content: Rope,

    /// Document version (incremented by editor on each change).
    /// We can use this to detect out-of-order updates.
    pub version: i32,
}

/// Why an incremental LSP change batch could not be applied safely.
///
/// Callers should keep the previous document snapshot when this error is
/// returned. [`Document::apply_changes_if_newer`] validates and applies changes
/// to a clone before replacing the tracked rope, so no variant represents a
/// partially applied batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentChangeError {
    /// The client supplied a newer document version without any content changes.
    EmptyChanges,
    /// A UTF-16 LSP position was outside the document or split a surrogate pair.
    InvalidPosition(Position),
    /// A change range ended before it started after UTF-16 conversion.
    ReversedRange { start: Position, end: Position },
}

impl fmt::Display for DocumentChangeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyChanges => write!(formatter, "document change contained no edits"),
            Self::InvalidPosition(position) => write!(
                formatter,
                "invalid UTF-16 document position {}:{}",
                position.line, position.character
            ),
            Self::ReversedRange { start, end } => write!(
                formatter,
                "document change range is reversed ({}:{} to {}:{})",
                start.line, start.character, end.line, end.character
            ),
        }
    }
}

impl std::error::Error for DocumentChangeError {}

impl Document {
    /// Replace the full document content when the client version is newer.
    ///
    /// Returns `true` when the replacement was accepted. Equal or older
    /// versions are ignored and return `false`, preserving the current content.
    pub fn replace_if_newer(&mut self, version: i32, content: &str) -> bool {
        if version <= self.version {
            return false;
        }

        self.content = Rope::from_str(content);
        self.version = version;
        true
    }

    /// Apply an ordered LSP change batch when the client version is newer.
    ///
    /// Positions use the negotiated UTF-16 encoding. The batch is applied to a
    /// clone and committed atomically so an invalid range cannot partially
    /// mutate the tracked document. Changes are interpreted in array order, so
    /// every range addresses the result of the preceding change.
    ///
    /// Returns `Ok(true)` after applying a newer version and `Ok(false)` for an
    /// equal or stale version. A newer but malformed batch returns
    /// `DocumentChangeError` and leaves both content and version unchanged.
    pub fn apply_changes_if_newer(
        &mut self,
        version: i32,
        changes: &[TextDocumentContentChangeEvent],
    ) -> Result<bool, DocumentChangeError> {
        if version <= self.version {
            return Ok(false);
        }
        if changes.is_empty() {
            return Err(DocumentChangeError::EmptyChanges);
        }

        let mut updated = self.content.clone();
        for change in changes {
            if let Some(range) = change.range {
                let start = Self::position_to_char(&updated, range.start)?;
                let end = Self::position_to_char(&updated, range.end)?;
                if start > end {
                    return Err(DocumentChangeError::ReversedRange {
                        start: range.start,
                        end: range.end,
                    });
                }

                updated.remove(start..end);
                updated.insert(start, &change.text);
            } else {
                updated = Rope::from_str(&change.text);
            }
        }

        self.content = updated;
        self.version = version;
        Ok(true)
    }

    /// Convert an LSP UTF-16 position into Ropey's Unicode-scalar index.
    ///
    /// Line terminators are not addressable as line content. Positions beyond
    /// the logical line, outside the document, or halfway through a non-BMP
    /// character return [`DocumentChangeError::InvalidPosition`].
    fn position_to_char(rope: &Rope, position: Position) -> Result<usize, DocumentChangeError> {
        let line_index = position.line as usize;
        if line_index >= rope.len_lines() {
            return Err(DocumentChangeError::InvalidPosition(position));
        }

        let line = rope.line(line_index);
        let line_start = rope.line_to_char(line_index);
        let target = position.character as usize;
        let mut utf16_offset = 0;
        let mut char_offset = 0;

        for character in line.chars() {
            if matches!(character, '\r' | '\n') {
                break;
            }
            if utf16_offset == target {
                return Ok(line_start + char_offset);
            }

            utf16_offset += character.len_utf16();
            if utf16_offset > target {
                return Err(DocumentChangeError::InvalidPosition(position));
            }
            char_offset += 1;
        }

        if utf16_offset == target {
            Ok(line_start + char_offset)
        } else {
            Err(DocumentChangeError::InvalidPosition(position))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_replacement_requires_a_newer_version() {
        let mut document = Document {
            content: Rope::from_str("version one"),
            version: 1,
        };

        assert!(!document.replace_if_newer(1, "duplicate"));
        assert!(!document.replace_if_newer(0, "stale"));
        assert_eq!(document.content.to_string(), "version one");
        assert_eq!(document.version, 1);

        assert!(document.replace_if_newer(3, "version three"));
        assert_eq!(document.content.to_string(), "version three");
        assert_eq!(document.version, 3);
    }

    #[test]
    fn applies_ordered_incremental_changes() {
        let mut document = Document {
            content: Rope::from_str("circuit main(): Field {\n    return 1;\n}\n"),
            version: 1,
        };
        let changes = vec![
            TextDocumentContentChangeEvent {
                range: Some(lsp_types::Range {
                    start: Position {
                        line: 1,
                        character: 11,
                    },
                    end: Position {
                        line: 1,
                        character: 12,
                    },
                }),
                range_length: Some(1),
                text: "2".to_string(),
            },
            TextDocumentContentChangeEvent {
                range: Some(lsp_types::Range {
                    start: Position {
                        line: 2,
                        character: 0,
                    },
                    end: Position {
                        line: 2,
                        character: 0,
                    },
                }),
                range_length: Some(0),
                text: "    // changed\n".to_string(),
            },
        ];

        assert!(document.apply_changes_if_newer(2, &changes).unwrap());
        assert_eq!(
            document.content.to_string(),
            "circuit main(): Field {\n    return 2;\n    // changed\n}\n"
        );
        assert_eq!(document.version, 2);
    }

    #[test]
    fn applies_utf16_ranges_without_splitting_surrogate_pairs() {
        let mut document = Document {
            content: Rope::from_str("😀 value"),
            version: 1,
        };
        let replace_value = TextDocumentContentChangeEvent {
            range: Some(lsp_types::Range {
                start: Position {
                    line: 0,
                    character: 3,
                },
                end: Position {
                    line: 0,
                    character: 8,
                },
            }),
            range_length: Some(5),
            text: "result".to_string(),
        };

        assert!(document
            .apply_changes_if_newer(2, &[replace_value])
            .unwrap());
        assert_eq!(document.content.to_string(), "😀 result");
    }

    #[test]
    fn rejects_invalid_ranges_without_partial_mutation() {
        let mut document = Document {
            content: Rope::from_str("😀 value"),
            version: 1,
        };
        let changes = vec![
            TextDocumentContentChangeEvent {
                range: Some(lsp_types::Range {
                    start: Position {
                        line: 0,
                        character: 3,
                    },
                    end: Position {
                        line: 0,
                        character: 8,
                    },
                }),
                range_length: Some(5),
                text: "result".to_string(),
            },
            TextDocumentContentChangeEvent {
                range: Some(lsp_types::Range {
                    start: Position {
                        line: 0,
                        character: 1,
                    },
                    end: Position {
                        line: 0,
                        character: 2,
                    },
                }),
                range_length: Some(1),
                text: String::new(),
            },
        ];

        assert_eq!(
            document.apply_changes_if_newer(2, &changes),
            Err(DocumentChangeError::InvalidPosition(Position {
                line: 0,
                character: 1,
            }))
        );
        assert_eq!(document.content.to_string(), "😀 value");
        assert_eq!(document.version, 1);
    }

    #[test]
    fn supports_full_replacement_inside_change_batches() {
        let mut document = Document {
            content: Rope::from_str("old"),
            version: 1,
        };
        let changes = vec![TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "new".to_string(),
        }];

        assert!(document.apply_changes_if_newer(2, &changes).unwrap());
        assert_eq!(document.content.to_string(), "new");
    }

    #[test]
    fn applies_positions_to_crlf_documents() {
        let mut document = Document {
            content: Rope::from_str("first\r\nsecond\r\n"),
            version: 1,
        };
        let insert = TextDocumentContentChangeEvent {
            range: Some(lsp_types::Range {
                start: Position {
                    line: 1,
                    character: 6,
                },
                end: Position {
                    line: 1,
                    character: 6,
                },
            }),
            range_length: Some(0),
            text: "!".to_string(),
        };

        assert!(document.apply_changes_if_newer(2, &[insert]).unwrap());
        assert_eq!(document.content.to_string(), "first\r\nsecond!\r\n");
    }

    #[test]
    fn incremental_changes_require_a_newer_version() {
        let mut document = Document {
            content: Rope::from_str("current"),
            version: 4,
        };
        let replacement = TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "stale".to_string(),
        };

        assert!(!document
            .apply_changes_if_newer(4, std::slice::from_ref(&replacement))
            .unwrap());
        assert!(!document.apply_changes_if_newer(3, &[replacement]).unwrap());
        assert_eq!(document.content.to_string(), "current");
        assert_eq!(document.version, 4);
    }
}
