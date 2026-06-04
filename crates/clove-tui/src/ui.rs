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

use crate::app::{fmt_day, fmt_ts, App, Detail, DetailTab, Focus, Mode, SortField, Tab};
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

    if app.mode == Mode::Filter {
        render_filter_menu(f, app, area);
    }
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
    let focused = app.focus == Focus::List;

    // Title shows visible/total when the view is narrowed by a filter or search.
    let narrowed = app.filter.is_active() || !app.search.is_empty();
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

    f.render_stateful_widget(list, area, &mut app.list_state);
}

/// One width-aware line in the item list: a status glyph, a single-letter type
/// icon, the short id (right-aligned in `id_w`), priority, the title, and a
/// ready/blocked badge. The title budget is computed from the actual pane width.
fn list_row(app: &App, fm: &ItemFrontmatter, inner_w: u16, id_w: usize) -> Line<'static> {
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
        format!("p{} ", fm.priority.get()),
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

fn render_detail(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Detail;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused))
        .padding(Padding::new(1, 1, 0, 0))
        .title(detail_title(app));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(detail) = &app.detail else {
        f.render_widget(
            Paragraph::new("No item selected.").style(Style::default().fg(DIM)),
            inner,
        );
        return;
    };

    // On the Overview tab, pin a 1-line footer (labels left, dates right) at the
    // bottom of the pane when the pane is wide (matching the wide body layout);
    // narrower panes inline labels/dates instead. The body scrolls above it.
    let footer = app.detail_tab == DetailTab::Overview && inner.width >= 50;
    let (body_area, footer_area) = if footer {
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .split(inner);
        (parts[0], Some(parts[1]))
    } else {
        (inner, None)
    };

    let lines = match app.detail_tab {
        DetailTab::Overview => overview_lines(app, detail, body_area.width, footer),
        DetailTab::Tree => tree_lines(detail),
        DetailTab::Comments => comment_lines(detail),
    };
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0)),
        body_area,
    );

    if let Some(fa) = footer_area {
        f.render_widget(
            Paragraph::new(footer_line(&detail.item.frontmatter, fa.width)),
            fa,
        );
    }
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

/// The overview. The header puts the short id, an ALL-CAPS type tag, and the
/// title on the top-left and status top-right; priority/assignee and a deps
/// count stack below the status (right) when wide. Then blockers, relationships
/// (deps are a count here, not a list — the Dep tree tab has the list), and a
/// trailing horizontal rule. When `footer` is set (wide), labels and dates live
/// in the pinned footer; otherwise they're emitted inline.
fn overview_lines(app: &App, detail: &Detail, inner_w: u16, footer: bool) -> Vec<Line<'static>> {
    let fm = &detail.item.frontmatter;
    let wide = inner_w >= 50;
    let mut lines = Vec::new();

    let id = short_ref(&fm.id);
    let type_tag = fm.item_type.as_str().to_uppercase();
    // id + spaces + TYPE + spaces, all before the title.
    let prefix_w = id.chars().count() + 2 + type_tag.chars().count() + 2;
    let head = |title: String| {
        vec![
            Span::styled(format!("{id}  "), Style::default().fg(LABEL)),
            Span::styled(
                format!("{type_tag}  "),
                type_style(fm.item_type).add_modifier(Modifier::BOLD),
            ),
            Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
        ]
    };

    if wide {
        // Title truncated so the header fits on one line beside the status.
        let status = status_spans(app, fm);
        let status_w: usize = status.iter().map(Span::width).sum();
        let title_budget = (inner_w as usize)
            .saturating_sub(prefix_w + status_w + 2)
            .max(8);
        lines.push(right_align(
            head(truncate(&fm.title, title_budget)),
            status,
            inner_w,
        ));
        // priority · assignee, then a deps count, stacked under the status.
        let mut meta = vec![Span::styled(
            format!("p{}", fm.priority.get()),
            priority_style(fm.priority.get()),
        )];
        if let Some(a) = &fm.assignee {
            meta.push(Span::styled(" · ", Style::default().fg(DIM)));
            meta.push(Span::styled(format!("@{a}"), Style::default().fg(LABEL)));
        }
        lines.push(right_align(vec![], meta, inner_w));
        if !fm.deps.is_empty() {
            lines.push(right_align(
                vec![],
                vec![Span::styled(
                    format!("deps {}", fm.deps.len()),
                    Style::default().fg(LABEL),
                )],
                inner_w,
            ));
        }
        lines.push(Line::raw(""));
    } else {
        // Narrow: the title is free to wrap (multi-line); fields stack as rows.
        lines.push(Line::from(head(fm.title.clone())));
        lines.push(Line::raw(""));
        lines.push(field_line("status", status_spans(app, fm)));
        lines.push(field_line(
            "priority",
            vec![Span::styled(
                format!("p{}", fm.priority.get()),
                priority_style(fm.priority.get()),
            )],
        ));
        if !fm.deps.is_empty() {
            lines.push(kv("deps", &fm.deps.len().to_string()));
        }
        if let Some(a) = &fm.assignee {
            lines.push(kv("assignee", a));
        }
    }

    // Blockers (decision-critical, kept near the top).
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

    // Labels + dates: inline rows only when there is no pinned footer (narrow);
    // wide panes show them in the footer instead.
    if !footer {
        if !fm.labels.is_empty() {
            lines.push(kv("labels", &fm.labels.join(", ")));
        }
        lines.push(time_field("created", fm.created));
        lines.push(time_field("updated", fm.updated));
        if let Some(c) = fm.closed {
            lines.push(time_field("closed", c));
        }
    }

    // Relationships (deps omitted — see the Dep tree tab / the deps count above).
    let mut rel = Vec::new();
    push_id_field(&mut rel, "parent", fm.parent.as_ref().into_iter());
    push_id_field(&mut rel, "relates", fm.relates.iter());
    push_id_field(&mut rel, "duplicates", fm.duplicates.iter());
    push_id_field(&mut rel, "supersedes", fm.supersedes.iter());
    if !rel.is_empty() {
        lines.push(Line::raw(""));
        lines.extend(rel);
    }

    // Body, rendered as Markdown under a plain (unlabeled) horizontal rule.
    let body = detail.item.body.trim();
    if !body.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "─".repeat(inner_w as usize),
            Style::default().fg(DIM),
        )));
        lines.extend(markdown::render(body, inner_w));
    }

    lines
}

/// The status glyph + word, plus a `· ready`/`· blocked` suffix.
fn status_spans(app: &App, fm: &ItemFrontmatter) -> Vec<Span<'static>> {
    let mut v = vec![
        Span::styled(status_glyph(fm.status), status_style(fm.status)),
        Span::raw(" "),
        Span::styled(fm.status.as_str().to_string(), status_style(fm.status)),
    ];
    if app.is_ready(&fm.id) {
        v.push(Span::styled(" · ", Style::default().fg(DIM)));
        v.push(Span::styled("ready", Style::default().fg(Color::Green)));
    } else if app.is_blocked(&fm.id) {
        v.push(Span::styled(" · ", Style::default().fg(DIM)));
        v.push(Span::styled("blocked", Style::default().fg(Color::Red)));
    }
    v
}

/// Build a line with `left` flush-left and `right` flush-right within `width`
/// (falls back to a single space between them if they don't both fit).
fn right_align(
    mut left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: u16,
) -> Line<'static> {
    let lw: usize = left.iter().map(Span::width).sum();
    let rw: usize = right.iter().map(Span::width).sum();
    let pad = (width as usize).saturating_sub(lw + rw).max(1);
    left.push(Span::raw(" ".repeat(pad)));
    left.extend(right);
    Line::from(left)
}

/// A day-resolution timestamp row (e.g. `created     Jan 20`).
fn time_field(key: &str, ts: chrono::DateTime<chrono::Utc>) -> Line<'static> {
    field_line(key, vec![Span::raw(fmt_day(ts))])
}

/// The short form of an id for display: drop the (per-repo, redundant) prefix
/// and trim leading zeros — e.g. `proj-00000042` → `42`, `proj-7af3q2k9` →
/// `7af3q2k9`.
fn short_id(id: &clove_core::CloveId) -> String {
    let s = id.as_str();
    let suffix = s.rsplit_once('-').map(|(_, b)| b).unwrap_or(s);
    let trimmed = suffix.trim_start_matches('0');
    if trimmed.is_empty() {
        "0".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `short_id` with a leading `#` so it reads as a reference.
fn short_ref(id: &clove_core::CloveId) -> String {
    format!("#{}", short_id(id))
}

/// The pinned Overview footer: labels flush-left (truncated with `+N` to make
/// room), created/updated (and `closed`) flush-right at day resolution.
fn footer_line(fm: &ItemFrontmatter, width: u16) -> Line<'static> {
    let mut right = vec![
        Span::styled("created ", Style::default().fg(DIM)),
        Span::raw(fmt_day(fm.created)),
        Span::styled(" · updated ", Style::default().fg(DIM)),
        Span::raw(fmt_day(fm.updated)),
    ];
    if let Some(c) = fm.closed {
        right.push(Span::styled(" · closed ", Style::default().fg(DIM)));
        right.push(Span::raw(fmt_day(c)));
    }
    let right_w: usize = right.iter().map(Span::width).sum();

    let mut left = Vec::new();
    if !fm.labels.is_empty() {
        let key = "labels ";
        let budget = (width as usize).saturating_sub(right_w + 2 + key.len());
        let (text, hidden) = fit_labels(&fm.labels, budget);
        left.push(Span::styled(key, Style::default().fg(DIM)));
        left.push(Span::raw(text));
        if hidden > 0 {
            left.push(Span::styled(
                format!(" +{hidden}"),
                Style::default().fg(DIM),
            ));
        }
    }
    right_align(left, right, width)
}

/// Join as many labels as fit in `budget` columns; return the text and the
/// count omitted.
fn fit_labels(labels: &[String], budget: usize) -> (String, usize) {
    let mut out = String::new();
    let mut shown = 0;
    for (i, l) in labels.iter().enumerate() {
        let sep = if i == 0 { "" } else { ", " };
        let add = sep.chars().count() + l.chars().count();
        if shown > 0 && out.chars().count() + add > budget {
            break;
        }
        out.push_str(sep);
        out.push_str(l);
        shown += 1;
    }
    (out, labels.len() - shown)
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
        short_ref(&node.id),
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
        Mode::Browse => {
            let mut spans = Vec::new();
            if !app.search.is_empty() {
                spans.push(Span::styled(
                    format!("search:{}  ", app.search),
                    Style::default().fg(Color::Yellow),
                ));
            }
            if app.filter.is_active() {
                spans.push(Span::styled(
                    format!("{}  ", filter_summary(app, narrow)),
                    Style::default().fg(Color::Yellow),
                ));
            }
            if app.sort.field != SortField::Default {
                spans.push(Span::styled(
                    format!("sort:{}{}  ", app.sort.field.label(), app.sort.dir.glyph()),
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
fn filter_summary(app: &App, narrow: bool) -> String {
    let f = &app.filter;
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

fn render_help(f: &mut Frame, area: Rect) {
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

/// The facet filter menu: facets grouped with headers, each value a radio
/// (single-valued facets) or checkbox (multi-valued), the cursor row marked.
fn render_filter_menu(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    let mut cursor_line: u16 = 0;
    if app.filter_menu.is_empty() {
        lines.push(Line::from(Span::styled(
            "no facets to filter",
            Style::default().fg(DIM),
        )));
    }

    let mut last_facet = None;
    for (i, item) in app.filter_menu.iter().enumerate() {
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
        let cursor = i == app.filter_cursor;
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
        .style(Style::default().bg(Color::Black));
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(block).scroll((scroll, 0)),
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
    ids.iter().map(short_ref).collect::<Vec<_>>().join(", ")
}

fn push_id_field<'a>(
    lines: &mut Vec<Line<'static>>,
    key: &str,
    ids: impl Iterator<Item = &'a clove_core::CloveId>,
) {
    let joined = ids.map(short_ref).collect::<Vec<_>>().join(", ");
    if !joined.is_empty() {
        lines.push(kv(key, &joined));
    }
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

/// A single-letter type icon (color carries the rest of the meaning).
fn type_icon(t: ItemType) -> char {
    match t {
        ItemType::Bug => 'B',
        ItemType::Feature => 'F',
        ItemType::Chore => 'C',
        ItemType::Docs => 'D',
        ItemType::Epic => 'E',
    }
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
