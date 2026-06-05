//! Filter-menu overlay rendering.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};
use ratatui::Frame;

use crate::app::App;

use super::style::{ACCENT, DIM, LABEL, SEL_BG};
use super::util::centered_fixed;

/// The facet filter menu: facets grouped with headers, each value a radio
/// (single-valued facets) or checkbox (multi-valued), the cursor row marked.
pub(crate) fn render_filter_menu(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    let mut cursor_line: u16 = 0;
    if app.filter_menu.menu.is_empty() {
        lines.push(Line::from(Span::styled(
            "no facets to filter",
            Style::default().fg(DIM),
        )));
    }

    let mut last_facet = None;
    for (i, item) in app.filter_menu.menu.iter().enumerate() {
        if last_facet != Some(item.facet) {
            if last_facet.is_some() {
                lines.push(Line::raw(""));
            }
            lines.push(Line::from(Span::styled(
                item.facet.label().to_string(),
                Style::default().fg(LABEL).add_modifier(Modifier::BOLD),
            )));
            last_facet = Some(item.facet);
        }

        let on = app.is_menu_selected(i);
        let mark = match (item.facet.is_single(), on) {
            (true, true) => "(•)",
            (true, false) => "( )",
            (false, true) => "[x]",
            (false, false) => "[ ]",
        };
        let cursor = i == app.filter_menu.cursor;
        if cursor {
            cursor_line = lines.len() as u16;
        }
        let pointer = if cursor { "▌" } else { " " };
        let row_style = if cursor {
            Style::default().bg(SEL_BG).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{pointer}{mark} "), row_style),
            Span::styled(item.text.clone(), row_style),
        ]));
    }

    let rows = lines.len() as u16;
    let popup = if area.width < 50 || area.height < 18 {
        area
    } else {
        let w = 40.min(area.width.saturating_sub(2));
        let h = (rows + 2).min(area.height.saturating_sub(2)).min(24);
        centered_fixed(area, w, h)
    };
    // Scroll so the cursor row stays visible when the menu exceeds the popup.
    let inner_h = popup.height.saturating_sub(2);
    let scroll = cursor_line.saturating_sub(inner_h.saturating_sub(1));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .padding(Padding::new(1, 1, 0, 0))
        .title(" Filter ")
        .style(Style::default().bg(ratatui::style::Color::Black));
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(block).scroll((scroll, 0)),
        popup,
    );
}
