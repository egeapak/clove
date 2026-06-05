//! Comments tab rendering.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::{fmt_ts, Detail};
use crate::ui::style::{ACCENT, DIM};

pub(crate) fn comment_lines(detail: &Detail) -> Vec<Line<'static>> {
    if detail.comments.is_empty() {
        return vec![Line::from(Span::styled(
            "No comments.",
            Style::default().fg(DIM),
        ))];
    }
    let mut lines = Vec::new();
    for (i, c) in detail.comments.iter().enumerate() {
        if i > 0 {
            lines.push(Line::raw(""));
        }
        lines.push(Line::from(vec![
            Span::styled(
                c.author.clone(),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(fmt_ts(c.timestamp), Style::default().fg(DIM)),
        ]));
        for raw in c.body.trim_end().lines() {
            lines.push(Line::raw(raw.to_string()));
        }
    }
    lines
}
