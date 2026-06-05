//! Tab bar rendering.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Tabs};
use ratatui::Frame;

use crate::app::{App, Tab};

use super::style::{ACCENT, DIM, LABEL};

pub(crate) fn render_tabs(f: &mut Frame, app: &App, area: Rect, compact: bool) {
    let narrow = area.width < 44;
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .map(|t| {
            let count = match t {
                Tab::All => app.total_count(),
                _ => app.visible_for(*t),
            };
            let name = if narrow { short_tab(*t) } else { t.title() };
            Line::from(format!(" {name} {count} "))
        })
        .collect();

    let block = if compact {
        // One-line, borderless: reclaim vertical space on short terminals.
        Block::default()
    } else {
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(DIM))
            .title(Span::styled(
                " clove ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ))
    };

    let tabs = Tabs::new(titles)
        .select(app.list.tab.index())
        .block(block)
        .style(Style::default().fg(LABEL))
        .highlight_style(
            Style::default()
                .fg(ratatui::style::Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
        )
        .divider(Span::styled("│", Style::default().fg(DIM)));
    f.render_widget(tabs, area);
}

fn short_tab(t: Tab) -> &'static str {
    match t {
        Tab::All => "All",
        Tab::Ready => "Rdy",
        Tab::Blocked => "Blk",
    }
}
