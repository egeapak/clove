//! Dependency tree tab rendering.

use clove_core::DepTreeNode;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::app::Detail;
use crate::ui::style::{status_glyph, status_style, DIM, LABEL};
use crate::ui::util::{short_ref, truncate};

/// The dependency tree, rendered with status glyphs + titles inline (children
/// sorted by id for stable output).
pub(crate) fn tree_lines(detail: &Detail) -> Vec<Line<'static>> {
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

pub(crate) fn push_tree_node(
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
