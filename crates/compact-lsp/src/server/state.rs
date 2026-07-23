//! Document state management.

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

impl Document {
    /// Replace the full document content when the client version is newer.
    pub fn replace_if_newer(&mut self, version: i32, content: &str) -> bool {
        if version <= self.version {
            return false;
        }

        self.content = Rope::from_str(content);
        self.version = version;
        true
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
}
