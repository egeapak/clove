//! Overview tab rendering (wide and narrow).

use clove_core::ItemFrontmatter;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::{fmt_day, App, Detail};
use crate::ui::style::{priority_glyph, priority_style, status_glyph, status_style, DIM, LABEL};
use crate::ui::util::{
    field_line, fit_labels, join_ids, kv, push_id_field, right_align, short_ref, truncate,
};

/// The header's meta line: id (`#42`) + priority glyph + ALL-CAPS type tag.
/// Shared by the wide and narrow headers so priority always reads from the same
/// place; the title lives on the line below.
pub(super) fn head_spans(fm: &ItemFrontmatter) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            format!("{}  ", short_ref(&fm.id)),
            Style::default().fg(LABEL),
        ),
        Span::styled(
            format!("{} ", priority_glyph(fm.priority.get())),
            priority_style(fm.priority.get()),
        ),
        Span::styled(
            fm.item_type.as_str().to_uppercase(),
            crate::ui::style::type_style(fm.item_type).add_modifier(Modifier::BOLD),
        ),
    ]
}

/// The title span, styled bold (its own line under the meta line).
pub(super) fn title_span(title: String) -> Span<'static> {
    Span::styled(title, Style::default().add_modifier(Modifier::BOLD))
}

/// `@assignee · deps N` spans (omitting whichever is absent), shown to the right
/// under the status in the wide header.
pub(super) fn assignee_deps_spans(fm: &ItemFrontmatter) -> Vec<Span<'static>> {
    let mut v = Vec::new();
    if let Some(a) = &fm.assignee {
        v.push(Span::styled(format!("@{a}"), Style::default().fg(LABEL)));
    }
    if !fm.deps.is_empty() {
        if !v.is_empty() {
            v.push(Span::styled(" · ", Style::default().fg(DIM)));
        }
        v.push(Span::styled(
            format!("deps {}", fm.deps.len()),
            Style::default().fg(LABEL),
        ));
    }
    v
}

/// Blocker/children rows shared by the wide and narrow overviews.
pub(super) fn blocker_lines(detail: &Detail, lines: &mut Vec<Line<'static>>) {
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
}

/// Relationship rows (deps omitted — the Dep tree tab has the list).
pub(super) fn relation_lines(fm: &ItemFrontmatter) -> Vec<Line<'static>> {
    let mut rel = Vec::new();
    push_id_field(&mut rel, "parent", fm.parent.as_ref());
    push_id_field(&mut rel, "relates", fm.relates.iter());
    push_id_field(&mut rel, "duplicates", fm.duplicates.iter());
    push_id_field(&mut rel, "supersedes", fm.supersedes.iter());
    rel
}

/// The wide Overview's fixed header (two lines before any blockers): line 1 is
/// id/priority/type with the status flush-right; line 2 is the title with the
/// assignee and deps count flush-right under the status. Sized to its content so
/// the body gets the rest of the pane.
pub(super) fn overview_header(app: &App, detail: &Detail, inner_w: u16) -> Vec<Line<'static>> {
    let fm = &detail.item.frontmatter;

    // The title shares line 2 with the assignee/deps, so it's truncated to the
    // width those leave free.
    let assignee_deps = assignee_deps_spans(fm);
    let ad_w: usize = assignee_deps.iter().map(Span::width).sum();
    let title_budget = (inner_w as usize).saturating_sub(ad_w + 2).max(8);

    let mut lines = vec![
        right_align(head_spans(fm), status_spans(app, fm), inner_w),
        right_align(
            vec![title_span(truncate(&fm.title, title_budget))],
            assignee_deps,
            inner_w,
        ),
    ];
    blocker_lines(detail, &mut lines);
    lines
}

/// The wide Overview's scrolling body: relationships then the Markdown body.
pub(super) fn overview_body(detail: &Detail, inner_w: u16) -> Vec<Line<'static>> {
    let fm = &detail.item.frontmatter;
    let mut lines = relation_lines(fm);
    let body = detail.item.body.trim();
    if !body.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.extend(crate::markdown::render(body, inner_w));
    }
    lines
}

/// The narrow Overview (single scrolling paragraph, no sticky footer): the title
/// may wrap, fields stack as rows, labels/dates inline, body under a plain rule.
pub(super) fn overview_lines(app: &App, detail: &Detail, inner_w: u16) -> Vec<Line<'static>> {
    let fm = &detail.item.frontmatter;

    let mut lines = vec![
        Line::from(head_spans(fm)),
        Line::from(title_span(fm.title.clone())),
        Line::raw(""),
        field_line("status", status_spans(app, fm)),
    ];
    if let Some(a) = &fm.assignee {
        lines.push(kv("assignee", &format!("@{a}")));
    }
    if !fm.deps.is_empty() {
        lines.push(kv("deps", &fm.deps.len().to_string()));
    }
    blocker_lines(detail, &mut lines);

    if !fm.labels.is_empty() {
        lines.push(kv("labels", &fm.labels.join(", ")));
    }
    lines.push(time_field("created", fm.created));
    lines.push(time_field("updated", fm.updated));
    if let Some(c) = fm.closed {
        lines.push(time_field("closed", c));
    }

    let rel = relation_lines(fm);
    if !rel.is_empty() {
        lines.push(Line::raw(""));
        lines.extend(rel);
    }

    let body = detail.item.body.trim();
    if !body.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "─".repeat(inner_w as usize),
            Style::default().fg(DIM),
        )));
        lines.extend(crate::markdown::render(body, inner_w));
    }

    lines
}

/// The status glyph + word, plus a `· ready`/`· blocked` suffix.
pub(super) fn status_spans(app: &App, fm: &ItemFrontmatter) -> Vec<Span<'static>> {
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

/// The pinned Overview footer: labels flush-left (truncated with `+N` to make
/// room), created/updated (and `closed`) flush-right at day resolution.
pub(super) fn footer_line(fm: &ItemFrontmatter, width: u16) -> Line<'static> {
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

/// A day-resolution timestamp row (e.g. `created     Jan 20`).
pub(super) fn time_field(key: &str, ts: chrono::DateTime<chrono::Utc>) -> Line<'static> {
    field_line(key, vec![Span::raw(fmt_day(ts))])
}
