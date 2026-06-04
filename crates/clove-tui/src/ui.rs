//! Rendering: an adaptive master-detail layout plus list/detail panes and the
//! search/help overlays.
//!
//! The body layout adapts to the terminal shape (see [`pick_layout`]):
//! side-by-side when wide, list-over-detail when tall-and-medium, and a single
//! focused pane when narrow or short. List rows and the status/tab bars also
//! degrade gracefully as space shrinks.

use clove_core::{DepTreeNode, ItemFrontmatter, ItemStatus, ItemType};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Padding, Paragraph, Tabs, Wrap};
use ratatui::Frame;

use crate::app::{fmt_relative, fmt_ts, App, Detail, DetailTab, Focus, Mode, Tab};
use crate::markdown;

// Structural chrome uses indexed grays (consistent on 256-color terminals);
// semantic foregrounds keep named ANSI colors so they respect the user's theme.
const ACCENT: Color = Color::Cyan;
const LABEL: Color = Color::Indexed(244);
const DIM: Color = Color::Indexed(240);
const SEL_BG: Color = Color::Indexed(236);

/// Below this height the tab bar collapses from a 3-line bordered bar to a
/// single borderless line, reclaiming two rows for the body.
const COMPACT_TAB_BELOW_H: u16 = 20;

/// How the body splits between the list and detail panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyLayout {
    /// Side-by-side: list left, detail right.
    Wide,
    /// Stacked: list above, detail below.
    Stacked,
    /// One pane at a time, chosen by [`App::focus`].
    Single,
    /// Too small to render usefully.
    TooSmall,
}

/// Choose a body layout from the available area (designer breakpoints):
/// Wide ≥ 80 cols; Stacked 50–79 cols and reasonably tall; otherwise Single;
/// TooSmall below a hard floor.
fn pick_layout(area: Rect) -> BodyLayout {
    if area.width < 24 || area.height < 6 {
        BodyLayout::TooSmall
    } else if area.width >= 80 {
        BodyLayout::Wide
    } else if area.width >= 50 && area.height >= 28 {
        BodyLayout::Stacked
    } else {
        BodyLayout::Single
    }
}

/// Render the whole frame.
pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if area.width < 24 || area.height < 6 {
        render_too_small(f, area);
        return;
    }

    let compact_tabs = area.height < COMPACT_TAB_BELOW_H;
    let tab_h = if compact_tabs { 1 } else { 3 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(tab_h), // tab bar
            Constraint::Min(1),        // body
            Constraint::Length(1),     // status / search line
        ])
        .split(area);

    render_tabs(f, app, chunks[0], compact_tabs);
    render_body(f, app, chunks[1]);
    render_status(f, app, chunks[2]);

    if app.show_help {
        render_help(f, area);
    }
}

fn render_too_small(f: &mut Frame, area: Rect) {
    let p = Paragraph::new("terminal too small")
        .style(Style::default().fg(Color::Yellow))
        .alignment(ratatui::layout::Alignment::Center);
    f.render_widget(p, area);
}

fn render_tabs(f: &mut Frame, app: &App, area: Rect, compact: bool) {
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
        .select(app.tab.index())
        .block(block)
        .style(Style::default().fg(LABEL))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
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

fn render_body(f: &mut Frame, app: &mut App, area: Rect) {
    match pick_layout(area) {
        BodyLayout::Wide => {
            // Give the list enough width to show the id when there's room to
            // spare; on tighter wide terminals keep it compact.
            let list_w = if area.width >= 100 { 48 } else { 40 };
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(list_w), Constraint::Min(38)])
                .split(area);
            render_list(f, app, cols[0]);
            render_detail(f, app, cols[1]);
        }
        BodyLayout::Stacked => {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(45), Constraint::Min(6)])
                .split(area);
            render_list(f, app, rows[0]);
            render_detail(f, app, rows[1]);
        }
        BodyLayout::Single => match app.focus {
            Focus::List => render_list(f, app, area),
            Focus::Detail => render_detail(f, app, area),
        },
        BodyLayout::TooSmall => render_too_small(f, area),
    }
}

fn render_list(f: &mut Frame, app: &mut App, area: Rect) {
    let inner_w = area.width.saturating_sub(2);
    let items: Vec<ListItem> = app
        .visible()
        .map(|fm| ListItem::new(list_row(app, fm, inner_w)))
        .collect();

    let focused = app.focus == Focus::List;
    let title = format!(" Items ({}) ", app.visible_count());
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style(focused))
                .padding(Padding::new(0, 1, 0, 0))
                .title(title),
        )
        .highlight_style(
            Style::default()
                .bg(SEL_BG)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▌");

    f.render_stateful_widget(list, area, &mut app.list_state);
}

/// One width-aware line in the item list. Columns drop as space shrinks:
/// `≥58`: glyph + id + p# + type + title; `≥40`: drop type; `<40`: glyph + p#
/// + title. The title budget is computed from the actual pane width.
fn list_row(app: &App, fm: &ItemFrontmatter, inner_w: u16) -> Line<'static> {
    let inner = inner_w as usize;
    let mut spans = vec![
        Span::styled(status_glyph(fm.status), status_style(fm.status)),
        Span::raw(" "),
    ];

    if inner >= 40 {
        spans.push(Span::styled(
            format!("{:<13}", fm.id.as_str()),
            Style::default().fg(LABEL),
        ));
    }
    spans.push(Span::styled(
        format!(" p{} ", fm.priority.get()),
        priority_style(fm.priority.get()),
    ));
    if inner >= 58 {
        spans.push(Span::styled(
            format!("{:<7} ", fm.item_type.as_str()),
            type_style(fm.item_type),
        ));
    }

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

fn render_detail(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Detail;
    let inner_w = area.width.saturating_sub(4); // borders + horizontal padding
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused))
        .padding(Padding::new(1, 1, 0, 0))
        .title(detail_title(app));

    let Some(detail) = &app.detail else {
        let p = Paragraph::new("No item selected.")
            .block(block)
            .style(Style::default().fg(DIM));
        f.render_widget(p, area);
        return;
    };

    let lines = match app.detail_tab {
        DetailTab::Overview => overview_lines(app, detail, inner_w, app.now),
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
            spans.push(Span::styled(" · ", Style::default().fg(DIM)));
        }
        let style = if *t == app.detail_tab {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(LABEL)
        };
        spans.push(Span::styled(t.title(), style));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// The overview, ordered for triage: identity → decision block (status, ready,
/// blockers, priority, assignee) → metadata → relationships → body.
fn overview_lines(
    app: &App,
    detail: &Detail,
    inner_w: u16,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<Line<'static>> {
    let fm = &detail.item.frontmatter;
    let mut lines = Vec::new();

    lines.push(Line::from(Span::styled(
        fm.title.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        fm.id.to_string(),
        Style::default().fg(LABEL),
    )));
    lines.push(Line::raw(""));

    // Decision block.
    let ready_badge = if app.is_ready(&fm.id) {
        Span::styled("ready", Style::default().fg(Color::Green))
    } else if app.is_blocked(&fm.id) {
        Span::styled("blocked", Style::default().fg(Color::Red))
    } else {
        Span::styled("—", Style::default().fg(DIM))
    };
    lines.push(field_line(
        "status",
        vec![
            Span::styled(fm.status.as_str().to_string(), status_style(fm.status)),
            Span::raw("   "),
            ready_badge,
        ],
    ));
    if !detail.blocking_deps.is_empty() {
        lines.push(field_line(
            "blocked by",
            vec![Span::styled(
                join_ids(&detail.blocking_deps),
                Style::default().fg(Color::Red),
            )],
        ));
    }
    if !detail.dangling_deps.is_empty() {
        lines.push(field_line(
            "dangling",
            vec![Span::styled(
                join_ids(&detail.dangling_deps),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )],
        ));
    }
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

    // Metadata.
    lines.push(Line::raw(""));
    lines.push(kv("type", fm.item_type.as_str()));
    if !fm.labels.is_empty() {
        lines.push(kv("labels", &fm.labels.join(", ")));
    }
    lines.push(time_field("created", fm.created, now));
    lines.push(time_field("updated", fm.updated, now));
    if let Some(c) = fm.closed {
        lines.push(time_field("closed", c, now));
    }

    // Relationships.
    let mut rel = Vec::new();
    push_id_field(&mut rel, "parent", fm.parent.as_ref().into_iter());
    push_id_field(&mut rel, "deps", fm.deps.iter());
    push_id_field(&mut rel, "relates", fm.relates.iter());
    push_id_field(&mut rel, "duplicates", fm.duplicates.iter());
    push_id_field(&mut rel, "supersedes", fm.supersedes.iter());
    if !rel.is_empty() {
        lines.push(Line::raw(""));
        lines.extend(rel);
    }

    // Body, rendered as Markdown.
    let body = detail.item.body.trim();
    if !body.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            divider("body", inner_w),
            Style::default().fg(DIM),
        )));
        lines.extend(markdown::render(body, inner_w));
    }

    lines
}

/// A timestamp field showing the absolute time plus a dim relative delta.
fn time_field(
    key: &str,
    ts: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> Line<'static> {
    field_line(
        key,
        vec![
            Span::raw(fmt_ts(ts)),
            Span::styled(
                format!("  ({})", fmt_relative(now, ts)),
                Style::default().fg(DIM),
            ),
        ],
    )
}

/// The dependency tree, rendered with status glyphs + titles inline (children
/// sorted by id for stable output).
fn tree_lines(detail: &Detail) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            "Dependency tree (this → its dependencies)",
            Style::default().fg(DIM),
        )),
        Line::raw(""),
    ];
    match &detail.tree {
        Some(root) => push_tree_node(&mut lines, root, String::new(), true, true),
        None => lines.push(Line::from(Span::styled(
            "(no dependency information)",
            Style::default().fg(DIM),
        ))),
    }
    lines
}

fn push_tree_node(
    lines: &mut Vec<Line<'static>>,
    node: &DepTreeNode,
    prefix: String,
    is_last: bool,
    is_root: bool,
) {
    let connector = if is_root {
        ""
    } else if is_last {
        "└── "
    } else {
        "├── "
    };

    let mut spans = vec![Span::styled(
        format!("{prefix}{connector}"),
        Style::default().fg(DIM),
    )];
    spans.push(Span::styled(
        status_glyph(node.status),
        status_style(node.status),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        node.id.to_string(),
        Style::default().fg(LABEL),
    ));
    spans.push(Span::raw("  "));
    spans.push(Span::raw(truncate(&node.title, 48)));
    if node.cycle_ref {
        spans.push(Span::styled(" (cycle)", Style::default().fg(Color::Red)));
    } else if node.ready {
        spans.push(Span::styled(" [ready]", Style::default().fg(Color::Green)));
    }
    lines.push(Line::from(spans));

    if node.cycle_ref {
        return;
    }

    let child_prefix = if is_root {
        String::new()
    } else if is_last {
        format!("{prefix}    ")
    } else {
        format!("{prefix}│   ")
    };

    let mut children: Vec<&DepTreeNode> = node.children.iter().collect();
    children.sort_by(|a, b| a.id.cmp(&b.id));
    let last = children.len().saturating_sub(1);
    for (i, child) in children.into_iter().enumerate() {
        push_tree_node(lines, child, child_prefix.clone(), i == last, false);
    }
}

fn comment_lines(detail: &Detail) -> Vec<Line<'static>> {
    if detail.comments.is_empty() {
        return vec![Line::from(Span::styled(
            "No comments.",
            Style::default().fg(DIM),
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
            Span::styled(fmt_ts(c.timestamp), Style::default().fg(DIM)),
        ]));
        for raw in c.body.trim_end().lines() {
            lines.push(Line::raw(raw.to_string()));
        }
    }
    lines
}

fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let narrow = area.width < 50;
    let line = match app.mode {
        Mode::Search => {
            let mut spans = vec![
                Span::styled(
                    "/",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::raw(app.search.clone()),
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
        Mode::Browse => {
            let mut spans = Vec::new();
            if !app.search.is_empty() {
                spans.push(Span::styled(
                    format!("filter:{}  ", app.search),
                    Style::default().fg(Color::Yellow),
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

fn render_help(f: &mut Frame, area: Rect) {
    let rows = [
        ("↑/k ↓/j", "move selection"),
        ("g / G", "jump to top / bottom"),
        ("Tab / 1 2 3", "All / Ready / Blocked"),
        ("o / t / c", "overview / dep tree / comments"),
        ("→/l  ←/h", "focus detail / list (narrow)"),
        ("PgUp / PgDn", "scroll detail"),
        ("/", "search id, title, labels"),
        ("Esc", "clear search / back / close"),
        ("r", "refresh from disk"),
        ("?", "toggle this help"),
        ("q", "quit"),
    ];

    let mut lines = vec![
        Line::from(Span::styled(
            "clove tui — read-only browser",
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

// --- small helpers --------------------------------------------------------

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(DIM)
    }
}

fn kv(key: &str, value: &str) -> Line<'static> {
    field_line(key, vec![Span::raw(value.to_string())])
}

fn field_line(key: &str, mut value: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{key:<11}"),
        Style::default().fg(LABEL),
    )];
    spans.append(&mut value);
    Line::from(spans)
}

fn join_ids(ids: &[clove_core::CloveId]) -> String {
    ids.iter()
        .map(|i| i.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn push_id_field<'a>(
    lines: &mut Vec<Line<'static>>,
    key: &str,
    ids: impl Iterator<Item = &'a clove_core::CloveId>,
) {
    let joined = ids.map(|i| i.as_str()).collect::<Vec<_>>().join(", ");
    if !joined.is_empty() {
        lines.push(kv(key, &joined));
    }
}

/// A full-width `── label ──────` rule sized to the pane.
fn divider(label: &str, inner_w: u16) -> String {
    let w = inner_w as usize;
    let head = format!("── {label} ");
    let fill = w.saturating_sub(head.chars().count());
    format!("{head}{}", "─".repeat(fill))
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
        // Closed reads as "done / out of the way" — gray, leaving green for ready.
        ItemStatus::Closed => Style::default().fg(LABEL),
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

/// Graded priority ramp: p0 hottest, p4 coldest.
fn priority_style(p: u8) -> Style {
    let color = match p {
        0 => Color::Indexed(196),
        1 => Color::Indexed(208),
        2 => Color::Indexed(178),
        3 => Color::Indexed(244),
        _ => Color::Indexed(240),
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

/// A `w`×`h` rectangle centered in `area`.
fn centered_fixed(area: Rect, w: u16, h: u16) -> Rect {
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w.min(area.width),
        height: h.min(area.height),
    }
}
