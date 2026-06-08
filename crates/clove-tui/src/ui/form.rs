//! Add/edit form overlay rendering.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};
use ratatui::Frame;

use crate::app::{App, Field, FormMode};

use super::style::{ACCENT, DIM, LABEL};
use super::util::centered_fixed;

/// The add/edit form: one row per field (text fields show a caret when focused,
/// enum fields show `‹ value ›`), an error line, and a key hint.
pub(crate) fn render_form(f: &mut Frame, app: &App, area: Rect) {
    let form = &app.form;
    let mut lines: Vec<Line> = Vec::new();
    let mut cursor_line: u16 = 0;

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
            // Body content below the label, indented, caret on the last line.
            let body = &form.body;
            let mut shown: Vec<&str> = body.split('\n').collect();
            if shown.last() == Some(&"") && shown.len() > 1 {
                shown.pop();
            }
            for (li, raw) in shown.iter().enumerate() {
                let last = li + 1 == shown.len();
                let mut text = format!("  {raw}");
                if focused && last {
                    text.push('▏');
                }
                lines.push(Line::from(Span::styled(
                    text,
                    Style::default().fg(Color::Reset),
                )));
            }
            if body.is_empty() {
                let caret = if focused { "▏" } else { "" };
                lines.push(Line::from(Span::styled(
                    format!("  {caret}"),
                    Style::default().fg(DIM),
                )));
            }
            continue;
        }

        let value: String = if field.is_enum() {
            let arrows = if focused { ("‹ ", " ›") } else { ("", "") };
            format!("{}{}{}", arrows.0, form.enum_value(field), arrows.1)
        } else {
            let mut v = text_value(form, field);
            if focused {
                v.push('▏');
            }
            v
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
        "Ctrl-S save · Esc cancel · Tab/↑↓ field · ←→ change",
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
    let scroll = cursor_line.saturating_sub(inner_h.saturating_sub(2));
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
