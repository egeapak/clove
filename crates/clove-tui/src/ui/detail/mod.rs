//! Detail pane rendering (overview, dependency tree, comments tabs).

mod comments;
mod overview;
mod tree;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, DetailTab, Focus};

use super::style::{ACCENT, DIM, LABEL};
use super::util::{border_style, render_rule};

use comments::comment_lines;
use overview::{footer_line, overview_body, overview_header, overview_lines};
use tree::tree_lines;

pub(crate) fn render_detail(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Detail;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused))
        .padding(Padding::new(1, 1, 0, 0))
        .title(detail_title(app));
    let inner = block.inner(area); // padded text area
    f.render_widget(block, area);

    let Some(detail) = &app.detail.detail else {
        f.render_widget(
            Paragraph::new("No item selected.").style(Style::default().fg(DIM)),
            inner,
        );
        return;
    };

    // Wide Overview: a fixed header and a sticky footer (labels + dates) bracket
    // a scrolling body, each separated by an edge-to-edge horizontal rule. Other
    // cases render a single scrolling paragraph.
    let wide_overview = app.detail.detail_tab == DetailTab::Overview && inner.width >= 50;
    if wide_overview {
        let header = overview_header(app, detail, inner.width);
        let body = overview_body(detail, inner.width);
        let footer = footer_line(&detail.item.frontmatter, inner.width);
        let zones = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(header.len() as u16), // header (shrunk to fit)
                Constraint::Length(1),                   // header rule
                Constraint::Min(1),                      // scrolling body
                Constraint::Length(1),                   // footer rule
                Constraint::Length(1),                   // sticky footer
            ])
            .split(inner);
        f.render_widget(Paragraph::new(header), zones[0]);
        render_rule(f, area, zones[1].y);
        f.render_widget(
            Paragraph::new(body)
                .wrap(Wrap { trim: false })
                .scroll((app.detail.detail_scroll, 0)),
            zones[2],
        );
        render_rule(f, area, zones[3].y);
        f.render_widget(Paragraph::new(footer), zones[4]);
        return;
    }

    let lines = match app.detail.detail_tab {
        DetailTab::Overview => overview_lines(app, detail, inner.width),
        DetailTab::Tree => tree_lines(detail),
        DetailTab::Comments => comment_lines(detail),
    };
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.detail.detail_scroll, 0)),
        inner,
    );
}

fn detail_title(app: &App) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (i, t) in [DetailTab::Overview, DetailTab::Tree, DetailTab::Comments]
        .iter()
        .enumerate()
    {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(DIM)));
        }
        let style = if *t == app.detail.detail_tab {
            Style::default()
                .fg(ACCENT)
                .add_modifier(ratatui::style::Modifier::BOLD)
        } else {
            Style::default().fg(LABEL)
        };
        spans.push(Span::styled(t.title(), style));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}
