//! Small parsing helpers shared across commands.

use chrono::{DateTime, Utc};
use clove_types::{CloveError, CloveId, ItemStatus, Priority};

/// The current time truncated to whole milliseconds (the canonical on-disk
/// timestamp precision; matches `ItemStore`'s internal truncation).
pub fn now_millis() -> DateTime<Utc> {
    clove_types::truncate_to_millis(Utc::now())
}

/// Parse and validate an item id argument.
pub fn parse_id(raw: &str) -> Result<CloveId, CloveError> {
    CloveId::new(raw)
}

/// Parse a status word into [`ItemStatus`] (delegates to the shared core parser).
pub fn parse_status(raw: &str) -> Result<ItemStatus, CloveError> {
    ItemStatus::parse(raw)
}

/// Parse and validate a priority 0–4.
pub fn parse_priority(raw: u8) -> Result<Priority, CloveError> {
    Priority::new(raw)
}

/// Whether `c` can change what a terminal shows beyond its own cell: C0/C1
/// controls and DEL (cursor movement, CSI/OSC sequences, carriage return), the
/// bidi controls, and the invisible separators.
fn is_terminal_control(c: char) -> bool {
    c.is_control()
        || ('\u{80}'..='\u{9f}').contains(&c)
        || matches!(c,
            '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}')
}

/// Item text made safe for one line of human (terminal) output.
///
/// Titles, labels, assignees and comment authors come from item files and from
/// synced trackers, so they are untrusted: left raw, an ESC or CR in a title
/// rewrites or hides lines of `clove ls`, and an OSC sequence can set the
/// terminal title or clipboard. Control characters are dropped. JSON output is
/// never passed through this — it carries the exact stored value.
pub fn term_line(value: &str) -> String {
    value.chars().filter(|c| !is_terminal_control(*c)).collect()
}

/// [`term_line`] for multi-line text (bodies, comments): newlines and tabs are
/// layout, so they are kept; every other control character is dropped.
pub fn term_text(value: &str) -> String {
    value
        .chars()
        .filter(|c| matches!(c, '\n' | '\t') || !is_terminal_control(*c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{term_line, term_text};

    #[test]
    fn term_line_drops_escape_and_cursor_controls() {
        assert_eq!(term_line("ok\u{1b}[2K\rforged"), "ok[2Kforged");
        assert_eq!(term_line("a\u{1b}]52;c;AAAA\u{7}b"), "a]52;c;AAAAb");
        assert_eq!(term_line("line\nbreak\ttab"), "linebreaktab");
        assert_eq!(term_line("rtl\u{202e}txt\u{9b}31m"), "rtltxt31m");
        assert_eq!(term_line("plain ünïcödé ✓"), "plain ünïcödé ✓");
    }

    #[test]
    fn term_text_keeps_newlines_and_tabs_only() {
        assert_eq!(term_text("a\n\tb\r\u{1b}[1mc"), "a\n\tb[1mc");
    }
}
