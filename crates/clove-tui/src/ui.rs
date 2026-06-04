//! Rendering: the master-detail layout, list/detail panes, search/help overlays.

use clove_core::{ItemFrontmatter, ItemStatus, ItemType};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Padding, Paragraph, Tabs, Wrap};
use ratatui::Frame;

use crate::app::{fmt_ts, App, DetailTab, Mode, Tab};

const ACCENT: Color = Color::Cyan;

/// Render the whole frame.
pub fn render(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // tab bar
            Constraint::Min(1),    // body
            Constraint::Length(1), // status / search line
        ])
        .split(f.area());

    render_tabs(f, app, chunks[0]);
    render_body(f, app, chunks[1]);
    render_status(f, app, chunks[2]);

    if app.show_help {
        render_help(f, f.area());
    }
}

fn render_tabs(f: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .map(|t| {
            let count = match t {
                Tab::All => app.total_count(),
                Tab::Ready | Tab::Blocked => {
                    // Cheap: count via the shared predicates over all items.
                    app.visible_for(*t)
                }
            };
            Line::from(format!(" {} ({}) ", t.title(), count))
        })
        .collect();

    let tabs = Tabs::new(titles)
        .select(app.tab.index())
        .block(Block::default().borders(Borders::ALL).title(Span::styled(
            " clove ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
        )
        .divider("│");
    f.render_widget(tabs, area);
}

fn render_body(f: &mut Frame, app: &mut App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(area);

    render_list(f, app, cols[0]);
    render_detail(f, app, cols[1]);
}

fn render_list(f: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = app
        .visible()
        .map(|fm| ListItem::new(list_row(app, fm)))
        .collect();

    let title = format!(" Items ({}) ", app.visible_count());
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(
            Style::default()
                .bg(Color::Indexed(238))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▌");

    f.render_stateful_widget(list, area, &mut app.list_state);
}

/// One line in the item list: status glyph, id, priority, type, title.
fn list_row(app: &App, fm: &ItemFrontmatter) -> Line<'static> {
    let mut spans = vec![
        Span::styled(status_glyph(fm.status), status_style(fm.status)),
        Span::raw(" "),
        Span::styled(
            format!("{:<11}", fm.id.as_str()),
            Style::default().fg(Color::Gray),
        ),
        Span::styled(
            format!(" p{} ", fm.priority.get()),
            priority_style(fm.priority.get()),
        ),
        Span::styled(
            format!("{:<7} ", fm.item_type.as_str()),
            type_style(fm.item_type),
        ),
        Span::raw(truncate(&fm.title, 60)),
    ];

    // A ready / blocked badge that is meaningful regardless of the active tab.
    if app.is_ready(&fm.id) {
        spans.push(Span::styled(" ●", Style::default().fg(Color::Green)));
    } else if app.is_blocked(&fm.id) {
        spans.push(Span::styled(" ⊘", Style::default().fg(Color::Red)));
    }
    Line::from(spans)
}

fn render_detail(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(detail_title(app));

    let Some(detail) = &app.detail else {
        let p = Paragraph::new("No item selected.")
            .block(block)
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    };

    let lines = match app.detail_tab {
        DetailTab::Overview => overview_lines(app, detail),
        DetailTab::Tree => tree_lines(detail),
        DetailTab::Comments => comment_lines(detail),
    };

    let p = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.detail_scroll, 0));
    f.render_widget(p, area);
}

fn detail_title(app: &App) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (i, t) in [DetailTab::Overview, DetailTab::Tree, DetailTab::Comments]
        .iter()
        .enumerate()
    {
        if i > 0 {
            spans.push(Span::raw(" · "));
        }
        let style = if *t == app.detail_tab {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(t.title(), style));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

fn overview_lines(app: &App, detail: &crate::app::Detail) -> Vec<Line<'static>> {
    let fm = &detail.item.frontmatter;
    let mut lines = Vec::new();

    lines.push(Line::from(Span::styled(
        fm.title.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        fm.id.to_string(),
        Style::default().fg(Color::Gray),
    )));
    lines.push(Line::raw(""));

    let ready_badge = if app.is_ready(&fm.id) {
        Span::styled("ready", Style::default().fg(Color::Green))
    } else if app.is_blocked(&fm.id) {
        Span::styled("blocked", Style::default().fg(Color::Red))
    } else {
        Span::styled("—", Style::default().fg(Color::DarkGray))
    };
    lines.push(field_line(
        "status",
        vec![
            Span::styled(fm.status.as_str().to_string(), status_style(fm.status)),
            Span::raw("   "),
            ready_badge,
        ],
    ));
    lines.push(kv("type", fm.item_type.as_str()));
    lines.push(field_line(
        "priority",
        vec![Span::styled(
            format!("p{}", fm.priority.get()),
            priority_style(fm.priority.get()),
        )],
    ));
    if let Some(a) = &fm.assignee {
        lines.push(kv("assignee", a));
    }
    if let Some(p) = &fm.parent {
        lines.push(kv("parent", p.as_str()));
    }
    if !fm.labels.is_empty() {
        lines.push(kv("labels", &fm.labels.join(", ")));
    }
    lines.push(kv("created", &fmt_ts(fm.created)));
    lines.push(kv("updated", &fmt_ts(fm.updated)));
    if let Some(c) = fm.closed {
        lines.push(kv("closed", &fmt_ts(c)));
    }

    if let Some(children) = &detail.children {
        lines.push(kv(
            "children",
            &format!(
                "{}/{} closed{}",
                children.closed,
                children.total,
                if children.completable {
                    " · completable"
                } else {
                    ""
                }
            ),
        ));
    }

    // Dependency relationships.
    push_id_list(&mut lines, "deps", &fm.deps);
    push_id_list(&mut lines, "relates", &fm.relates);
    push_id_list(&mut lines, "duplicates", &fm.duplicates);
    push_id_list(&mut lines, "supersedes", &fm.supersedes);

    if !detail.blocking_deps.is_empty() {
        let ids = detail
            .blocking_deps
            .iter()
            .map(|i| i.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(field_line(
            "blocked by",
            vec![Span::styled(ids, Style::default().fg(Color::Red))],
        ));
    }
    if !detail.dangling_deps.is_empty() {
        let ids = detail
            .dangling_deps
            .iter()
            .map(|i| i.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(field_line(
            "dangling",
            vec![Span::styled(
                ids,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )],
        ));
    }

    // Body.
    let body = detail.item.body.trim();
    if !body.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "── body ──",
            Style::default().fg(Color::DarkGray),
        )));
        for raw in body.lines() {
            lines.push(Line::raw(raw.to_string()));
        }
    }

    lines
}

fn tree_lines(detail: &crate::app::Detail) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        "Dependency tree (this → its dependencies)",
        Style::default().fg(Color::DarkGray),
    ))];
    lines.push(Line::raw(""));
    for raw in detail.tree.lines() {
        let style = if raw.contains("(cycle)") {
            Style::default().fg(Color::Red)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(raw.to_string(), style)));
    }
    lines
}

fn comment_lines(detail: &crate::app::Detail) -> Vec<Line<'static>> {
    if detail.comments.is_empty() {
        return vec![Line::from(Span::styled(
            "No comments.",
            Style::default().fg(Color::DarkGray),
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
            Span::styled(fmt_ts(c.timestamp), Style::default().fg(Color::DarkGray)),
        ]));
        for raw in c.body.trim_end().lines() {
            lines.push(Line::raw(raw.to_string()));
        }
    }
    lines
}

fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let line = match app.mode {
        Mode::Search => Line::from(vec![
            Span::styled(
                "/",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(app.search.clone()),
            Span::styled("▏", Style::default().fg(ACCENT)),
            Span::styled(
                "   (Enter: keep · Esc: clear)",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Mode::Browse => {
            let mut spans = Vec::new();
            if !app.search.is_empty() {
                spans.push(Span::styled(
                    format!("filter:{}  ", app.search),
                    Style::default().fg(Color::Yellow),
                ));
            }
            spans.push(Span::styled(&app.status, Style::default().fg(Color::Gray)));
            spans.push(Span::styled(
                "   ?: help · q: quit",
                Style::default().fg(Color::DarkGray),
            ));
            Line::from(spans)
        }
    };
    f.render_widget(Paragraph::new(line), area);
}

fn render_help(f: &mut Frame, area: Rect) {
    let rows = [
        ("↑/k  ↓/j", "move selection"),
        ("g / G", "jump to top / bottom"),
        ("Tab / 1 2 3", "switch All / Ready / Blocked"),
        ("o / t / c", "detail: overview / dep tree / comments"),
        ("PgUp / PgDn", "scroll detail pane"),
        ("/", "search (id, title, labels)"),
        ("Esc", "clear search / close help"),
        ("r", "refresh from disk"),
        ("? ", "toggle this help"),
        ("q", "quit"),
    ];

    let mut lines = vec![Line::from(Span::styled(
        "clove tui — read-only browser",
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ))];
    lines.push(Line::raw(""));
    for (keys, desc) in rows {
        lines.push(Line::from(vec![
            Span::styled(format!("  {keys:<14}"), Style::default().fg(Color::Yellow)),
            Span::raw(desc),
        ]));
    }

    let popup = centered_rect(58, 60, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::uniform(1))
        .title(" Help ")
        .style(Style::default().bg(Color::Black));
    f.render_widget(Clear, popup);
    f.render_widget(Paragraph::new(lines).block(block), popup);
}

// --- small helpers --------------------------------------------------------

fn kv(key: &str, value: &str) -> Line<'static> {
    field_line(key, vec![Span::raw(value.to_string())])
}

fn field_line(key: &str, mut value: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{key:<11}"),
        Style::default().fg(Color::DarkGray),
    )];
    spans.append(&mut value);
    Line::from(spans)
}

fn push_id_list(lines: &mut Vec<Line<'static>>, key: &str, ids: &[clove_core::CloveId]) {
    if ids.is_empty() {
        return;
    }
    let joined = ids
        .iter()
        .map(|i| i.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    lines.push(kv(key, &joined));
}

fn status_glyph(s: ItemStatus) -> &'static str {
    match s {
        ItemStatus::Open => "○",
        ItemStatus::InProgress => "◐",
        ItemStatus::Closed => "●",
    }
}

fn status_style(s: ItemStatus) -> Style {
    match s {
        ItemStatus::Open => Style::default().fg(Color::Yellow),
        ItemStatus::InProgress => Style::default().fg(Color::Cyan),
        ItemStatus::Closed => Style::default().fg(Color::Green),
    }
}

fn type_style(t: ItemType) -> Style {
    let color = match t {
        ItemType::Bug => Color::Red,
        ItemType::Feature => Color::Blue,
        ItemType::Chore => Color::Gray,
        ItemType::Docs => Color::Magenta,
        ItemType::Epic => Color::Yellow,
    };
    Style::default().fg(color)
}

fn priority_style(p: u8) -> Style {
    let color = match p {
        0 => Color::Red,
        1 => Color::LightRed,
        2 => Color::Yellow,
        _ => Color::DarkGray,
    };
    Style::default().fg(color)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// A centered rectangle `pct_x` × `pct_y` percent of `area`.
fn centered_rect(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(vertical[1])[1]
}
