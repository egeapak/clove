//! Add/edit form overlay rendering.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};
use ratatui::Frame;

use crate::app::{App, Field, FormMode};

use super::style::{ACCENT, DIM, LABEL};
use super::util::centered_fixed;

/// The width of a field label column: pointer (1) + a 9-wide right-justified
/// `Name:` + a trailing space. All field names are ≤ 8 chars, so `{:>9}` is
/// always exactly 9 columns and this constant holds.
const LABEL_COLS: u16 = 11;

/// The add/edit form: one row per field (enum fields show `‹ value ›`), an error
/// line, and a key hint. The focused text field's caret is shown via the
/// terminal's **hardware cursor** (placed on the caret cell); for cursor-less
/// backends — the snapshot tests + PNG screenshots — `app.caret_glyph` also draws
/// an in-buffer `▏` glyph at the same cell. Live terminals leave the glyph off so
/// the two don't double up.
pub(crate) fn render_form(f: &mut Frame, app: &App, area: Rect) {
    let form = &app.form;
    let mut lines: Vec<Line> = Vec::new();
    let mut cursor_line: u16 = 0;
    // The caret's absolute (pre-scroll) position within the content: `(line, col)`.
    // `None` while an enum field is focused (no text caret).
    let mut caret: Option<(u16, u16)> = None;

    for (i, &field) in form.fields.iter().enumerate() {
        let focused = i == form.focus;
        if focused {
            cursor_line = lines.len() as u16;
        }
        let pointer = if focused { "▌" } else { " " };
        let label = format!("{pointer}{:>9} ", format!("{}:", field.label()));
        let label_style = if focused {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(LABEL)
        };

        if field == Field::Body {
            lines.push(Line::from(Span::styled(label, label_style)));
            // Body content below the label, indented; the caret is drawn inline at
            // the cursor position (it may land mid-text or on its own new line).
            if focused {
                let before = &form.body[..char_byte(&form.body, form.cursor)];
                let lines_before = before.matches('\n').count() as u16;
                let col = before.rsplit('\n').next().unwrap_or("").chars().count() as u16;
                caret = Some((cursor_line + 1 + lines_before, 2 + col));
            }
            let body = if focused && app.caret_glyph {
                with_caret(&form.body, form.cursor)
            } else {
                form.body.clone()
            };
            let mut shown: Vec<&str> = body.split('\n').collect();
            if shown.last() == Some(&"") && shown.len() > 1 {
                shown.pop();
            }
            for raw in &shown {
                lines.push(Line::from(Span::styled(
                    format!("  {raw}"),
                    Style::default().fg(Color::Reset),
                )));
            }
            if shown.is_empty() {
                lines.push(Line::from(Span::styled("  ", Style::default().fg(DIM))));
            }
            continue;
        }

        let value: String = if field.is_enum() {
            let arrows = if focused { ("‹ ", " ›") } else { ("", "") };
            format!("{}{}{}", arrows.0, form.enum_value(field), arrows.1)
        } else if focused {
            caret = Some((cursor_line, LABEL_COLS + form.cursor as u16));
            if app.caret_glyph {
                with_caret(&text_value(form, field), form.cursor)
            } else {
                text_value(form, field)
            }
        } else {
            text_value(form, field)
        };
        let value_style = if focused {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Reset)
        };
        lines.push(Line::from(vec![
            Span::styled(label, label_style),
            Span::styled(value, value_style),
        ]));
    }

    if let Some(err) = &form.error {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!("  {err}"),
            Style::default().fg(Color::Red),
        )));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "Ctrl-S save · Esc cancel · Tab/↑↓ field · ←→ move/change",
        Style::default().fg(DIM),
    )));

    let title = match form.mode {
        FormMode::New => " New item ".to_owned(),
        FormMode::Edit => format!(
            " Edit {} ",
            form.edit_id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_default()
        ),
    };

    let rows = lines.len() as u16;
    let popup = if area.width < 56 || area.height < 16 {
        area
    } else {
        let w = 64.min(area.width.saturating_sub(2));
        let h = (rows + 2).min(area.height.saturating_sub(2)).min(30);
        centered_fixed(area, w, h)
    };
    let inner_h = popup.height.saturating_sub(2);
    // Scroll so the caret (or, for enum fields, the focused row) stays visible.
    let focus_line = caret.map(|(l, _)| l).unwrap_or(cursor_line);
    let scroll = focus_line.saturating_sub(inner_h.saturating_sub(2));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .padding(Padding::new(1, 1, 0, 0))
        .title(title)
        .style(Style::default().bg(Color::Black));
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(block).scroll((scroll, 0)),
        popup,
    );

    // Place the terminal's hardware cursor on the caret cell (coincides with the
    // glyph) when it's within the visible, scrolled content area — so real
    // terminals show a native blinking cursor, while the glyph keeps the caret
    // visible in the (cursor-less) snapshot/PNG tooling.
    if let Some((line, col)) = caret {
        let inner_x = popup.x + 2; // left border + left padding
        let inner_y = popup.y + 1; // top border (no top padding)
        let inner_w = popup.width.saturating_sub(4);
        if line >= scroll && line < scroll + inner_h && col < inner_w {
            f.set_cursor_position((inner_x + col, inner_y + (line - scroll)));
        }
    }
}

/// The byte offset of char index `idx` in `s` (clamped to `s.len()`).
fn char_byte(s: &str, idx: usize) -> usize {
    s.char_indices().nth(idx).map(|(b, _)| b).unwrap_or(s.len())
}

/// Insert the caret glyph `▏` at char index `cursor` (clamped), so a moved
/// cursor renders mid-text. At end-of-string this appends the caret.
fn with_caret(s: &str, cursor: usize) -> String {
    let byte = char_byte(s, cursor);
    let mut out = String::with_capacity(s.len() + 3);
    out.push_str(&s[..byte]);
    out.push('▏');
    out.push_str(&s[byte..]);
    out
}

/// The current text buffer for a non-enum field.
fn text_value(form: &crate::app::FormState, field: Field) -> String {
    match field {
        Field::Title => form.title.clone(),
        Field::Assignee => form.assignee.clone(),
        Field::Labels => form.labels.clone(),
        Field::Parent => form.parent.clone(),
        Field::Deps => form.deps.clone(),
        _ => String::new(),
    }
}
