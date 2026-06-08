//! Semantic colour and glyph functions for status, type, and priority.

use clove_types::{ItemStatus, ItemType};
use ratatui::style::{Color, Style};

// Structural chrome uses indexed grays (consistent on 256-color terminals);
// semantic foregrounds keep named ANSI colors so they respect the user's theme.
pub(crate) const ACCENT: Color = Color::Cyan;
pub(crate) const LABEL: Color = Color::Indexed(244);
pub(crate) const DIM: Color = Color::Indexed(240);
pub(crate) const SEL_BG: Color = Color::Indexed(236);

pub(crate) fn status_glyph(s: ItemStatus) -> &'static str {
    match s {
        ItemStatus::Open => "○",
        ItemStatus::InProgress => "◐",
        ItemStatus::Closed => "●",
    }
}

pub(crate) fn status_style(s: ItemStatus) -> Style {
    match s {
        ItemStatus::Open => Style::default().fg(Color::Yellow),
        ItemStatus::InProgress => Style::default().fg(Color::Cyan),
        // Closed reads as "done / out of the way" — gray, leaving green for ready.
        ItemStatus::Closed => Style::default().fg(LABEL),
    }
}

pub(crate) fn type_style(t: ItemType) -> Style {
    let color = match t {
        ItemType::Bug => Color::Red,
        ItemType::Feature => Color::Blue,
        ItemType::Chore => Color::Gray,
        ItemType::Docs => Color::Magenta,
        ItemType::Epic => Color::Yellow,
    };
    Style::default().fg(color)
}

/// A single-letter type icon (color carries the rest of the meaning).
pub(crate) fn type_icon(t: ItemType) -> char {
    match t {
        ItemType::Bug => 'B',
        ItemType::Feature => 'F',
        ItemType::Chore => 'C',
        ItemType::Docs => 'D',
        ItemType::Epic => 'E',
    }
}

/// Graded priority ramp: p0 hottest, p4 coldest.
pub(crate) fn priority_style(p: u8) -> Style {
    let color = match p {
        0 => Color::Indexed(196),
        1 => Color::Indexed(208),
        2 => Color::Indexed(178),
        // p3 shares the `•` with p2 and is set apart by a dim icy blue.
        3 => Color::Indexed(110),
        // Lowest priority (`↓`): the coolest, dimmest gray.
        _ => Color::Indexed(244),
    };
    Style::default().fg(color)
}

/// A single-glyph priority indicator; color carries the rest of the meaning so
/// the two `•` levels (normal/low) are told apart by their amber-vs-gray hue:
/// `!` critical, `↑` high, `•` normal (p2) / low (p3), `↓` lowest (dim blue).
/// Out-of-range values fall back to `pN`.
pub(crate) fn priority_glyph(p: u8) -> String {
    match p {
        0 => "!".to_owned(),
        1 => "↑".to_owned(),
        2 | 3 => "•".to_owned(),
        4 => "↓".to_owned(),
        n => format!("p{n}"),
    }
}

#[cfg(test)]
mod tests {
    //! Colour-semantics tests. The render snapshots (`snapshot.rs`) flatten the
    //! buffer to plain text and so can't catch a colour regression; these lock
    //! the fg the style functions assign, with no layout/positions involved.
    use super::*;

    #[test]
    fn priority_ramp_is_hot_to_cold() {
        // p0 hottest → p4 coldest; p2 and p3 share the `•` glyph and are told
        // apart only by hue (amber vs dim icy blue), so their colours must differ.
        let fg = |p| priority_style(p).fg;
        assert_eq!(fg(0), Some(Color::Indexed(196))); // red
        assert_eq!(fg(1), Some(Color::Indexed(208))); // orange
        assert_eq!(fg(2), Some(Color::Indexed(178))); // amber
        assert_eq!(fg(3), Some(Color::Indexed(110))); // dim icy blue
        assert_eq!(fg(4), Some(Color::Indexed(244))); // gray
        assert_ne!(fg(2), fg(3));
        // Out-of-range priorities reuse the coldest end of the ramp.
        assert_eq!(fg(9), fg(4));
    }

    #[test]
    fn status_colours_match_glyphs() {
        assert_eq!(status_style(ItemStatus::Open).fg, Some(Color::Yellow));
        assert_eq!(status_style(ItemStatus::InProgress).fg, Some(Color::Cyan));
        // Closed reads as "done / out of the way" — gray, leaving green for ready.
        assert_eq!(status_style(ItemStatus::Closed).fg, Some(LABEL));
    }

    #[test]
    fn type_colours_are_distinct_per_kind() {
        let fg = |t| type_style(t).fg;
        assert_eq!(fg(ItemType::Bug), Some(Color::Red));
        assert_eq!(fg(ItemType::Feature), Some(Color::Blue));
        assert_eq!(fg(ItemType::Chore), Some(Color::Gray));
        assert_eq!(fg(ItemType::Docs), Some(Color::Magenta));
        assert_eq!(fg(ItemType::Epic), Some(Color::Yellow));
    }
}
