//! Small layout/rendering helpers shared across the ui modules.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::style::{ACCENT, DIM, LABEL};

/// Draw a horizontal rule spanning the full interior width (touching the side
/// borders, no padding gaps) at row `y` of detail pane `area`.
pub(crate) fn render_rule(f: &mut Frame, area: Rect, y: u16) {
    let w = area.width.saturating_sub(2);
    let rect = Rect {
        x: area.x + 1,
        y,
        width: w,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(w as usize),
            Style::default().fg(DIM),
        ))),
        rect,
    );
}

/// Build a line with `left` flush-left and `right` flush-right within `width`
/// (falls back to a single space between them if they don't both fit).
pub(crate) fn right_align(
    mut left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: u16,
) -> Line<'static> {
    let lw: usize = left.iter().map(Span::width).sum();
    let rw: usize = right.iter().map(Span::width).sum();
    let pad = (width as usize).saturating_sub(lw + rw).max(1);
    left.push(Span::raw(" ".repeat(pad)));
    left.extend(right);
    Line::from(left)
}

pub(crate) fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(DIM)
    }
}

pub(crate) fn kv(key: &str, value: &str) -> Line<'static> {
    field_line(key, vec![Span::raw(value.to_string())])
}

pub(crate) fn field_line(key: &str, mut value: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{key:<11}"),
        Style::default().fg(LABEL),
    )];
    spans.append(&mut value);
    Line::from(spans)
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// A `w`×`h` rectangle centered in `area`.
pub(crate) fn centered_fixed(area: Rect, w: u16, h: u16) -> Rect {
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w.min(area.width),
        height: h.min(area.height),
    }
}

/// The short form of an id for display: drop the (per-repo, redundant) prefix
/// and trim leading zeros — e.g. `proj-00000042` → `42`, `proj-7af3q2k9` →
/// `7af3q2k9`.
pub(crate) fn short_id(id: &clove_types::CloveId) -> String {
    let s = id.as_str();
    let suffix = s.rsplit_once('-').map(|(_, b)| b).unwrap_or(s);
    let trimmed = suffix.trim_start_matches('0');
    if trimmed.is_empty() {
        "0".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `short_id` with a leading `#` so it reads as a reference.
pub(crate) fn short_ref(id: &clove_types::CloveId) -> String {
    format!("#{}", short_id(id))
}

pub(crate) fn join_ids<'a>(ids: impl IntoIterator<Item = &'a clove_types::CloveId>) -> String {
    ids.into_iter()
        .map(short_ref)
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn push_id_field<'a>(
    lines: &mut Vec<Line<'static>>,
    key: &str,
    ids: impl IntoIterator<Item = &'a clove_types::CloveId>,
) {
    let joined = join_ids(ids);
    if !joined.is_empty() {
        lines.push(kv(key, &joined));
    }
}

/// Join as many labels as fit in `budget` columns; return the text and the
/// count omitted.
pub(crate) fn fit_labels(labels: &[String], budget: usize) -> (String, usize) {
    let mut out = String::new();
    let mut shown = 0;
    for (i, l) in labels.iter().enumerate() {
        let sep = if i == 0 { "" } else { ", " };
        let add = sep.chars().count() + l.chars().count();
        if shown > 0 && out.chars().count() + add > budget {
            break;
        }
        out.push_str(sep);
        out.push_str(l);
        shown += 1;
    }
    (out, labels.len() - shown)
}
