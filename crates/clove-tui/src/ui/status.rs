//! Status bar rendering (bottom line: search, filter, sort, hint).

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, FormMode, Mode, SortField};

use super::style::{ACCENT, DIM, LABEL};

pub(crate) fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let narrow = area.width < 50;
    let line = match app.mode {
        Mode::Search => {
            let mut spans = vec![
                Span::styled(
                    "/",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::raw(app.list.search.clone()),
                Span::styled("▏", Style::default().fg(ACCENT)),
            ];
            if !narrow {
                spans.push(Span::styled(
                    "   (Enter: keep · Esc: clear)",
                    Style::default().fg(DIM),
                ));
            }
            Line::from(spans)
        }
        Mode::Filter => {
            let mut spans = vec![Span::styled(
                "filter",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )];
            if !narrow {
                spans.push(Span::styled(
                    "  ↑↓ move · space toggle · x clear · Esc close",
                    Style::default().fg(DIM),
                ));
            }
            Line::from(spans)
        }
        Mode::Form => {
            let label = match app.form.mode {
                FormMode::New => "new item",
                FormMode::Edit => "edit item",
            };
            let mut spans = vec![Span::styled(
                label,
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )];
            if !narrow {
                spans.push(Span::styled(
                    "  Ctrl-S save · Esc cancel",
                    Style::default().fg(DIM),
                ));
            }
            Line::from(spans)
        }
        Mode::Browse => {
            let mut spans = Vec::new();
            if !app.list.search.is_empty() {
                spans.push(Span::styled(
                    format!("search:{}  ", app.list.search),
                    Style::default().fg(Color::Yellow),
                ));
            }
            if app.list.filter.is_active() {
                spans.push(Span::styled(
                    format!("{}  ", filter_summary(app, narrow)),
                    Style::default().fg(Color::Yellow),
                ));
            }
            if app.list.sort.field != SortField::Default {
                spans.push(Span::styled(
                    format!(
                        "sort:{}{}  ",
                        app.list.sort.field.label(),
                        app.list.sort.dir.glyph()
                    ),
                    Style::default().fg(ACCENT),
                ));
            }
            if app.is_busy() {
                spans.push(Span::styled(
                    format!("{} ", app.spinner()),
                    Style::default().fg(ACCENT),
                ));
            }
            spans.push(Span::styled(app.status.clone(), Style::default().fg(LABEL)));
            let hint = if narrow {
                "  ?·q"
            } else {
                "   ?: help · q: quit"
            };
            spans.push(Span::styled(hint, Style::default().fg(DIM)));
            Line::from(spans)
        }
    };
    f.render_widget(Paragraph::new(line), area);
}

/// A compact one-line summary of the active facet filters for the status bar.
pub(crate) fn filter_summary(app: &App, narrow: bool) -> String {
    let f = &app.list.filter;
    if narrow {
        let n = [
            f.status.is_some(),
            f.assignee.is_some(),
            !f.types.is_empty(),
            !f.priorities.is_empty(),
            !f.labels.is_empty(),
        ]
        .iter()
        .filter(|b| **b)
        .count();
        return format!("filters:{n}");
    }
    let mut parts = Vec::new();
    if let Some(s) = f.status {
        parts.push(format!("status:{}", s.as_str()));
    }
    if !f.types.is_empty() {
        let v = f
            .types
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>()
            .join(",");
        parts.push(format!("type:{v}"));
    }
    if !f.priorities.is_empty() {
        let mut ps: Vec<u8> = f.priorities.clone();
        ps.sort_unstable();
        let v = ps
            .iter()
            .map(|p| format!("p{p}"))
            .collect::<Vec<_>>()
            .join(",");
        parts.push(v);
    }
    if let Some(a) = &f.assignee {
        parts.push(format!("@{a}"));
    }
    if !f.labels.is_empty() {
        let mut ls = f.labels.clone();
        ls.sort();
        parts.push(format!("label:{}", ls.join(",")));
    }
    parts.join(" ")
}
