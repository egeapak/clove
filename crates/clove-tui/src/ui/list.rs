//! List pane rendering.

use clove_core::ItemFrontmatter;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Padding, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, Focus};

use super::style::{
    priority_glyph, priority_style, status_glyph, status_style, type_icon, type_style, DIM, LABEL,
    SEL_BG,
};
use super::util::{border_style, short_ref, truncate};

pub(crate) fn render_list(f: &mut Frame, app: &mut App, area: Rect) {
    let inner_w = area.width.saturating_sub(2);
    let focused = app.focus == Focus::List;

    // Title shows visible/total when the view is narrowed by a filter or search.
    let narrowed = app.list.filter.is_active() || !app.list.search.is_empty();
    let title = if narrowed {
        format!(" Items ({}/{}) ", app.visible_count(), app.total_count())
    } else {
        format!(" Items ({}) ", app.visible_count())
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused))
        .padding(Padding::new(0, 1, 0, 0))
        .title(title);

    // Distinguish "filtered to empty" (escape hatch) from "no items at all".
    if app.visible_count() == 0 && app.total_count() > 0 {
        let p = Paragraph::new(vec![
            Line::from(Span::styled(
                "No items match.",
                Style::default().fg(Color::Yellow),
            )),
            Line::raw(""),
            Line::from(Span::styled(
                "press x to clear filters, Esc to clear search",
                Style::default().fg(DIM),
            )),
        ])
        .block(block)
        .wrap(Wrap { trim: false });
        f.render_widget(p, area);
        return;
    }

    // Size the short-id column to the widest visible ref, so titles stay aligned.
    let id_w = app
        .visible()
        .map(|fm| short_ref(&fm.id).chars().count())
        .max()
        .unwrap_or(2)
        .clamp(2, 10);
    let items: Vec<ListItem> = app
        .visible()
        .map(|fm| ListItem::new(list_row(app, fm, inner_w, id_w)))
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(SEL_BG)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▌");

    f.render_stateful_widget(list, area, &mut app.list.list_state);
}

/// One width-aware line in the item list: a status glyph, a single-letter type
/// icon, the short id (right-aligned in `id_w`), priority, the title, and a
/// ready/blocked badge. The title budget is computed from the actual pane width.
pub(crate) fn list_row(
    app: &App,
    fm: &ItemFrontmatter,
    inner_w: u16,
    id_w: usize,
) -> Line<'static> {
    let inner = inner_w as usize;
    let mut spans = vec![
        Span::styled(status_glyph(fm.status), status_style(fm.status)),
        Span::raw(" "),
        Span::styled(
            type_icon(fm.item_type).to_string(),
            type_style(fm.item_type).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{:>id_w$} ", short_ref(&fm.id)),
            Style::default().fg(LABEL),
        ),
    ];
    spans.push(Span::styled(
        format!("{} ", priority_glyph(fm.priority.get())),
        priority_style(fm.priority.get()),
    ));

    // Reserve room for the trailing ready/blocked badge, then fit the title.
    let used: usize = spans.iter().map(|s| s.width()).sum();
    let title_budget = inner.saturating_sub(used + 2).max(6);
    spans.push(Span::raw(truncate(&fm.title, title_budget)));

    if app.is_ready(&fm.id) {
        spans.push(Span::styled(" ●", Style::default().fg(Color::Green)));
    } else if app.is_blocked(&fm.id) {
        spans.push(Span::styled(" ✗", Style::default().fg(Color::Red)));
    }
    Line::from(spans)
}
