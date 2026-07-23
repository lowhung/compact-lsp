//! Text analysis utilities.

use lsp_types::{Position, Range};

fn line_at(content: &str, line: u32) -> Option<&str> {
    content
        .split('\n')
        .nth(line as usize)
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
}

fn utf16_to_char_index(line: &str, utf16_column: u32) -> Option<usize> {
    let target = utf16_column as usize;
    let mut utf16_offset = 0;

    for (char_index, character) in line.chars().enumerate() {
        if utf16_offset == target {
            return Some(char_index);
        }

        utf16_offset += character.len_utf16();
        if utf16_offset > target {
            return None;
        }
    }

    (utf16_offset == target).then(|| line.chars().count())
}

fn char_index_to_utf16(line: &str, char_index: usize) -> Option<u32> {
    let mut utf16_offset = 0;

    for (index, character) in line.chars().enumerate() {
        if index == char_index {
            return Some(utf16_offset);
        }
        utf16_offset += character.len_utf16() as u32;
    }

    (char_index == line.chars().count()).then_some(utf16_offset)
}

/// Return the LSP position immediately after the final character in a document.
pub fn document_end_position(content: &str) -> Position {
    let mut lines = content.split('\n');
    let mut last_line = lines.next().unwrap_or("");
    let mut line = 0;

    for next_line in lines {
        line += 1;
        last_line = next_line;
    }

    let last_line = last_line.strip_suffix('\r').unwrap_or(last_line);
    Position {
        line,
        character: last_line.encode_utf16().count() as u32,
    }
}

/// Get the word (identifier) at a given position in the source.
pub fn get_word_at_position(content: &str, line: u32, character: u32) -> Option<String> {
    let line_content = line_at(content, line)?;
    let char_idx = utf16_to_char_index(line_content, character)?;

    // Find word boundaries (identifiers can contain a-z, A-Z, 0-9, _)
    let chars: Vec<char> = line_content.chars().collect();

    // Find start of word
    let mut start = char_idx;
    while start > 0 {
        let c = chars.get(start - 1)?;
        if c.is_alphanumeric() || *c == '_' {
            start -= 1;
        } else {
            break;
        }
    }

    // Find end of word
    let mut end = char_idx;
    while end < chars.len() {
        let c = chars.get(end)?;
        if c.is_alphanumeric() || *c == '_' {
            end += 1;
        } else {
            break;
        }
    }

    if start == end {
        return None;
    }

    Some(chars[start..end].iter().collect())
}

/// Get the range of the word at a given position.
pub fn get_word_range_at_position(content: &str, line: u32, character: u32) -> Option<Range> {
    let line_content = line_at(content, line)?;
    let char_idx = utf16_to_char_index(line_content, character)?;

    let chars: Vec<char> = line_content.chars().collect();

    // Find word boundaries
    let mut start = char_idx;
    while start > 0 {
        let c = chars.get(start - 1)?;
        if c.is_alphanumeric() || *c == '_' {
            start -= 1;
        } else {
            break;
        }
    }

    let mut end = char_idx;
    while end < chars.len() {
        let c = chars.get(end)?;
        if c.is_alphanumeric() || *c == '_' {
            end += 1;
        } else {
            break;
        }
    }

    if start == end {
        return None;
    }

    Some(Range {
        start: Position {
            line,
            character: char_index_to_utf16(line_content, start)?,
        },
        end: Position {
            line,
            character: char_index_to_utf16(line_content, end)?,
        },
    })
}

/// Get the function name from a call context (e.g., "func_name(" before cursor).
pub fn get_function_call_name(content: &str, line: u32, character: u32) -> Option<String> {
    let line_content = line_at(content, line)?;

    // Search backwards from cursor to find the opening paren
    let chars: Vec<char> = line_content.chars().collect();
    let mut idx = utf16_to_char_index(line_content, character)?;

    // First, find the opening paren of our call (handling nested parens)
    let mut paren_depth = 0;
    while idx > 0 {
        idx -= 1;
        match chars.get(idx) {
            Some(')') => paren_depth += 1,
            Some('(') => {
                if paren_depth == 0 {
                    // Found our opening paren - now get the identifier before it
                    break;
                }
                paren_depth -= 1;
            }
            _ => {}
        }
    }

    // Now idx points to '(' - search backwards for the function name
    if idx == 0 {
        return None;
    }

    // Skip any whitespace
    while idx > 0
        && chars
            .get(idx - 1)
            .map(|c| c.is_whitespace())
            .unwrap_or(false)
    {
        idx -= 1;
    }

    // Now find the identifier
    let end = idx;
    while idx > 0
        && chars
            .get(idx - 1)
            .map(|c| c.is_alphanumeric() || *c == '_')
            .unwrap_or(false)
    {
        idx -= 1;
    }

    if idx == end {
        return None;
    }

    Some(chars[idx..end].iter().collect())
}

/// Count commas before cursor position in a function call.
pub fn count_commas_before_cursor(content: &str, line: u32, character: u32) -> u32 {
    let line_content = match line_at(content, line) {
        Some(line) => line,
        None => return 0,
    };

    let chars: Vec<char> = line_content.chars().collect();
    let cursor_idx = match utf16_to_char_index(line_content, character) {
        Some(index) => index,
        None => return 0,
    };
    let mut comma_count = 0;
    let mut paren_depth = 0;

    // Search backwards from cursor to find commas (not inside nested parens)
    let mut idx = cursor_idx;
    while idx > 0 {
        idx -= 1;
        match chars.get(idx) {
            Some(')') => paren_depth += 1,
            Some('(') => {
                if paren_depth == 0 {
                    // Found our function call's opening paren
                    break;
                }
                paren_depth -= 1;
            }
            Some(',') if paren_depth == 0 => comma_count += 1,
            _ => {}
        }
    }

    comma_count
}

/// Parse parameter strings from a detail string like "(a: Field, b: Field): ReturnType".
pub fn parse_params_from_detail(detail: &str) -> Vec<String> {
    // Extract content between first '(' and matching ')'
    let start = match detail.find('(') {
        Some(i) => i + 1,
        None => return vec![],
    };

    let end = match detail.find(')') {
        Some(i) => i,
        None => return vec![],
    };

    if start >= end {
        return vec![];
    }

    let params_str = &detail[start..end];
    if params_str.trim().is_empty() {
        return vec![];
    }

    // Split by comma, handling potential nested types
    params_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Detect dot-completion context by scanning backwards from cursor.
///
/// Unlike `get_dot_access_variable` (which requires cursor immediately after the dot),
/// this handles partial identifiers after the dot (e.g., `round.inc|` returns `Some("round")`).
/// Works for both `round.|` and `round.inc|` positions.
pub fn detect_dot_context(content: &str, line: u32, character: u32) -> Option<String> {
    let line_content = line_at(content, line)?;
    let char_idx = utf16_to_char_index(line_content, character)?;

    let chars: Vec<char> = line_content.chars().collect();

    // Skip backwards over any partial identifier (e.g., the "inc" in "round.inc|")
    let mut idx = char_idx;
    while idx > 0 {
        let c = chars.get(idx - 1)?;
        if c.is_alphanumeric() || *c == '_' {
            idx -= 1;
        } else {
            break;
        }
    }

    // Now check if there's a dot
    if idx == 0 {
        return None;
    }
    if chars.get(idx - 1).copied() != Some('.') {
        return None;
    }
    let dot_idx = idx - 1;

    if dot_idx == 0 {
        return None;
    }

    // Scan backwards from before the dot to find the base identifier
    let end = dot_idx;
    let mut start = end;
    while start > 0 {
        let c = chars.get(start - 1)?;
        if c.is_alphanumeric() || *c == '_' {
            start -= 1;
        } else {
            break;
        }
    }

    if start == end {
        return None;
    }

    Some(chars[start..end].iter().collect())
}

/// Get the variable name before a dot at the cursor position.
///
/// If the cursor is right after `round.`, this returns `Some("round")`.
pub fn get_dot_access_variable(content: &str, line: u32, character: u32) -> Option<String> {
    let line_content = line_at(content, line)?;
    let char_idx = utf16_to_char_index(line_content, character)?;

    if char_idx == 0 {
        return None;
    }

    let chars: Vec<char> = line_content.chars().collect();

    // The character before the cursor should be '.'
    let dot_idx = char_idx - 1;
    if chars.get(dot_idx).copied() != Some('.') {
        return None;
    }

    if dot_idx == 0 {
        return None;
    }

    // Scan backwards from before the dot to find the identifier
    let end = dot_idx;
    let mut start = end;
    while start > 0 {
        let c = chars.get(start - 1)?;
        if c.is_alphanumeric() || *c == '_' {
            start -= 1;
        } else {
            break;
        }
    }

    if start == end {
        return None;
    }

    Some(chars[start..end].iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_word_at_position() {
        let content = "circuit add(a: Field, b: Field): Field {";
        assert_eq!(
            get_word_at_position(content, 0, 10),
            Some("add".to_string())
        );
        assert_eq!(
            get_word_at_position(content, 0, 0),
            Some("circuit".to_string())
        );
        assert_eq!(
            get_word_at_position(content, 0, 15),
            Some("Field".to_string())
        );
    }

    #[test]
    fn test_get_word_at_position_start_of_line() {
        let content = "myFunction()";
        assert_eq!(
            get_word_at_position(content, 0, 0),
            Some("myFunction".to_string())
        );
    }

    #[test]
    fn test_get_word_at_position_end_of_word() {
        let content = "myFunction()";
        assert_eq!(
            get_word_at_position(content, 0, 10),
            Some("myFunction".to_string())
        );
    }

    #[test]
    fn test_get_word_at_position_empty() {
        let content = "a + b";
        assert_eq!(get_word_at_position(content, 0, 2), None);
    }

    #[test]
    fn test_get_word_range_at_position() {
        let content = "circuit add(a: Field): Field {";
        let range = get_word_range_at_position(content, 0, 9).unwrap();
        assert_eq!(range.start.character, 8);
        assert_eq!(range.end.character, 11);
    }

    #[test]
    fn positions_are_utf16_code_units() {
        let content = "😀 add";
        let range = get_word_range_at_position(content, 0, 4).unwrap();

        assert_eq!(get_word_at_position(content, 0, 4), Some("add".into()));
        assert_eq!(range.start.character, 3);
        assert_eq!(range.end.character, 6);
        assert_eq!(
            get_dot_access_variable("😀 round.", 0, 9),
            Some("round".into())
        );
    }

    #[test]
    fn rejects_position_inside_surrogate_pair() {
        assert_eq!(get_word_at_position("😀 add", 0, 1), None);
    }

    #[test]
    fn document_end_uses_utf16_and_preserves_trailing_lines() {
        assert_eq!(
            document_end_position("first\n😀"),
            Position {
                line: 1,
                character: 2
            }
        );
        assert_eq!(
            document_end_position("first\n"),
            Position {
                line: 1,
                character: 0
            }
        );
    }

    #[test]
    fn test_parse_params_from_detail() {
        let detail = "(a: Field, b: Field): Field";
        let params = parse_params_from_detail(detail);
        assert_eq!(params, vec!["a: Field", "b: Field"]);
    }

    #[test]
    fn test_parse_params_from_detail_empty() {
        let detail = "(): Void";
        let params = parse_params_from_detail(detail);
        assert!(params.is_empty());
    }

    #[test]
    fn test_get_dot_access_variable() {
        // "round." with cursor at position 6 (after the dot)
        let content = "round.";
        assert_eq!(
            get_dot_access_variable(content, 0, 6),
            Some("round".to_string())
        );
    }

    #[test]
    fn test_get_dot_access_variable_in_context() {
        let content = "    round.";
        assert_eq!(
            get_dot_access_variable(content, 0, 10),
            Some("round".to_string())
        );
    }

    #[test]
    fn test_get_dot_access_variable_no_dot() {
        let content = "round";
        assert_eq!(get_dot_access_variable(content, 0, 5), None);
    }

    #[test]
    fn test_get_dot_access_variable_no_identifier() {
        let content = ".";
        assert_eq!(get_dot_access_variable(content, 0, 1), None);
    }

    #[test]
    fn test_detect_dot_context_right_after_dot() {
        // "round." with cursor at position 6 (after the dot)
        let content = "round.";
        assert_eq!(detect_dot_context(content, 0, 6), Some("round".to_string()));
    }

    #[test]
    fn test_detect_dot_context_partial_identifier() {
        // "round.inc" with cursor at position 9 (after "inc")
        let content = "round.inc";
        assert_eq!(detect_dot_context(content, 0, 9), Some("round".to_string()));
    }

    #[test]
    fn test_detect_dot_context_mid_partial() {
        // "round.increment" with cursor in middle of "increment"
        let content = "round.increment";
        assert_eq!(detect_dot_context(content, 0, 8), Some("round".to_string()));
    }

    #[test]
    fn test_detect_dot_context_indented() {
        let content = "    round.inc";
        assert_eq!(
            detect_dot_context(content, 0, 13),
            Some("round".to_string())
        );
    }

    #[test]
    fn test_detect_dot_context_no_dot() {
        let content = "round";
        assert_eq!(detect_dot_context(content, 0, 5), None);
    }

    #[test]
    fn test_detect_dot_context_no_base() {
        let content = ".inc";
        assert_eq!(detect_dot_context(content, 0, 4), None);
    }

    #[test]
    fn test_detect_dot_context_multiline() {
        let content =
            "ledger round: Counter;\nexport circuit incr(): [] {\n  round.increment(1);\n}";
        // Line 2: "  round.increment(1);" - cursor on "increment" at col 8
        assert_eq!(detect_dot_context(content, 2, 8), Some("round".to_string()));
    }
}
