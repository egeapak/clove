//! Help overlay rendering.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap};
use ratatui::Frame;

use super::style::{ACCENT, LABEL};
use super::util::centered_fixed;

pub(crate) fn render_help(f: &mut Frame, area: Rect) {
    let rows = [
        ("↑/k ↓/j", "move selection"),
        ("g / G", "jump to top / bottom"),
        ("Tab / 1 2 3", "All / Ready / Blocked"),
        ("o / t / c", "overview / dep tree / comments"),
        ("→/l  ←/h", "focus detail / list (narrow)"),
        ("PgUp / PgDn", "scroll detail"),
        ("s / S", "cycle sort field / direction"),
        ("f", "filter menu (facets)"),
        ("x", "clear all filters"),
        ("/", "search id, title, labels"),
        ("n / e", "new / edit item"),
        ("Esc", "clear search / back / close"),
        ("r", "refresh from disk"),
        ("?", "toggle this help"),
        ("q", "quit"),
    ];

    let mut lines = vec![
        Line::from(Span::styled(
            "clove tui — browser + editor",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
    ];
    for (keys, desc) in rows {
        lines.push(Line::from(vec![
            Span::styled(format!("{keys:<13}"), Style::default().fg(Color::Yellow)),
            Span::raw(desc),
        ]));
    }
    // Priority glyph legend.
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(format!("{:<13}", "priority"), Style::default().fg(LABEL)),
        Span::raw("! ↑ • ↓  =  p0 p1 p2/p3 p4 (by color)"),
    ]));

    // Content-sized and centered when there's room; a full-screen modal on
    // small terminals (where a centered box would be all border and no room).
    let popup = if area.width < 50 || area.height < 18 {
        area
    } else {
        let w = 50.min(area.width.saturating_sub(2));
        let h = (rows.len() as u16 + 4).min(area.height.saturating_sub(2));
        centered_fixed(area, w, h)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .padding(Padding::new(1, 1, 0, 0))
        .title(" Help ")
        .style(Style::default().bg(Color::Black));
    f.render_widget(Clear, popup);
    // Wrap so descriptions never clip mid-word on a narrow full-screen help.
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        popup,
    );
}
