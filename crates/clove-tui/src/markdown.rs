//! A compact CommonMark → ratatui renderer for the item body.
//!
//! Built on `pulldown-cmark`, so inline emphasis/code, nested lists, code
//! blocks, block quotes, tables, and rules parse correctly. Output is a `Vec<Line>`
//! styled for a terminal; soft line breaks become spaces so paragraphs reflow
//! under the `Paragraph` widget's word wrap, while hard breaks and block
//! boundaries start new lines.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::Indexed(240);
const CODE: Color = Color::Indexed(180);

/// Render Markdown `md` to styled lines, sizing rules to `width`.
pub fn render(md: &str, width: u16) -> Vec<Line<'static>> {
    let mut r = Renderer::new(width);
    let parser = Parser::new_ext(md, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES);
    for event in parser {
        r.event(event);
    }
    r.finish()
}

#[derive(Default)]
struct Renderer {
    width: u16,
    lines: Vec<Line<'static>>,
    cur: Vec<Span<'static>>,
    bold: u32,
    italic: u32,
    strike: u32,
    heading: bool,
    /// Open links, innermost last: the destination and where the link text
    /// starts in `cur` (to tell whether the text already is the URL).
    links: Vec<(String, usize)>,
    code_block: bool,
    /// The rows of the table being read (`None` outside a table). Each row is
    /// its cells; each cell is the spans it rendered to. Rows are laid out at
    /// the table's end, when every column's width is known.
    table: Option<Vec<Vec<Vec<Span<'static>>>>>,
    /// Per-nesting-level list state: `Some(n)` ordered (next number), `None` bullet.
    list_stack: Vec<Option<u64>>,
    quote: u32,
}

impl Renderer {
    fn new(width: u16) -> Self {
        Renderer {
            width,
            ..Default::default()
        }
    }

    fn inline_style(&self) -> Style {
        let mut s = Style::default();
        if self.heading {
            s = s.fg(ACCENT).add_modifier(Modifier::BOLD);
        }
        if !self.links.is_empty() {
            s = s.fg(ACCENT).add_modifier(Modifier::UNDERLINED);
        }
        if self.bold > 0 {
            s = s.add_modifier(Modifier::BOLD);
        }
        if self.italic > 0 {
            s = s.add_modifier(Modifier::ITALIC);
        }
        if self.strike > 0 {
            s = s.add_modifier(Modifier::CROSSED_OUT);
        }
        s
    }

    /// Flush the current spans as a line, prefixed with any block-quote marker.
    fn end_line(&mut self) {
        if self.cur.is_empty() {
            return;
        }
        let mut spans = Vec::new();
        if self.quote > 0 {
            spans.push(Span::styled(
                "│ ".repeat(self.quote as usize),
                Style::default().fg(DIM),
            ));
        }
        spans.append(&mut self.cur);
        self.lines.push(Line::from(spans));
    }

    /// Push a blank separator unless the previous line is already blank/empty.
    fn blank(&mut self) {
        if self.lines.last().map(|l| l.width() != 0).unwrap_or(false) {
            self.lines.push(Line::raw(""));
        }
    }

    fn push_text(&mut self, text: &str) {
        if self.code_block {
            for (i, seg) in text.split('\n').enumerate() {
                if i > 0 {
                    self.end_line();
                }
                // An empty segment still contributes a span: `end_line`
                // no-ops on an empty `cur`, so skipping it entirely would
                // swallow blank lines inside the code block and fuse the
                // surrounding lines together.
                let content = if seg.is_empty() {
                    String::new()
                } else {
                    format!("    {seg}")
                };
                self.cur
                    .push(Span::styled(content, Style::default().fg(CODE)));
            }
        } else {
            let style = self.inline_style();
            self.cur.push(Span::styled(text.to_owned(), style));
        }
    }

    fn start_item(&mut self) {
        let depth = self.list_stack.len();
        let indent = "  ".repeat(depth.saturating_sub(1));
        let marker = match self.list_stack.last_mut() {
            Some(Some(n)) => {
                let m = format!("{indent}{n}. ");
                *n += 1;
                m
            }
            _ => format!("{indent}• "),
        };
        self.cur
            .push(Span::styled(marker, Style::default().fg(DIM)));
    }

    /// Close a link. The destination is shown after the text, dimmed, when the
    /// text is not already the URL: a terminal cannot follow a link, so a
    /// destination that is not printed is lost.
    fn end_link(&mut self) {
        let Some((dest, start)) = self.links.pop() else {
            return;
        };
        let text: String = self.cur[start.min(self.cur.len())..]
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        let bare = dest.strip_prefix("mailto:").unwrap_or(&dest);
        if !dest.is_empty() && text != dest && text != bare {
            self.cur
                .push(Span::styled(format!(" ({dest})"), Style::default().fg(DIM)));
        }
    }

    /// Lay out the buffered table: one line per row, cells padded to their
    /// column's width and joined by `│`, with a rule under the header row.
    fn end_table(&mut self) {
        let Some(rows) = self.table.take() else {
            return;
        };
        let cell_width =
            |cell: &Vec<Span<'_>>| -> usize { cell.iter().map(|s| s.content.width()).sum() };
        let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
        let widths: Vec<usize> = (0..cols)
            .map(|c| {
                rows.iter()
                    .filter_map(|row| row.get(c))
                    .map(cell_width)
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let rule = Style::default().fg(DIM);
        for (r, row) in rows.into_iter().enumerate() {
            let mut cells = row.into_iter();
            for (c, width) in widths.iter().enumerate() {
                if c > 0 {
                    self.cur.push(Span::styled(" │ ", rule));
                }
                let cell = cells.next().unwrap_or_default();
                let pad = width.saturating_sub(cell_width(&cell));
                for mut span in cell {
                    if r == 0 {
                        span.style = span.style.add_modifier(Modifier::BOLD);
                    }
                    self.cur.push(span);
                }
                // No trailing padding after the last column.
                if c + 1 < cols && pad > 0 {
                    self.cur.push(Span::raw(" ".repeat(pad)));
                }
            }
            // `end_line` skips an empty `cur`; a row of empty cells is still a row.
            if self.cur.is_empty() {
                self.cur.push(Span::raw(""));
            }
            self.end_line();
            if r == 0 {
                let line = widths
                    .iter()
                    .map(|w| "─".repeat(*w))
                    .collect::<Vec<_>>()
                    .join("─┼─");
                self.cur.push(Span::styled(line, rule));
                self.end_line();
            }
        }
        self.blank();
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { .. } => self.heading = true,
                Tag::BlockQuote(_) => self.quote += 1,
                Tag::CodeBlock(_) => {
                    self.end_line();
                    self.code_block = true;
                }
                Tag::List(start) => {
                    // A list nested in an item starts on its own line; without
                    // this the first nested marker joined the parent's text
                    // (`• a  • nested`).
                    self.end_line();
                    self.list_stack.push(start);
                }
                Tag::Item => self.start_item(),
                Tag::Emphasis => self.italic += 1,
                Tag::Strong => self.bold += 1,
                Tag::Strikethrough => self.strike += 1,
                Tag::Link { dest_url, .. } => {
                    self.links.push((dest_url.to_string(), self.cur.len()));
                }
                Tag::Table(_) => {
                    self.end_line();
                    self.table = Some(Vec::new());
                }
                Tag::TableHead | Tag::TableRow => {
                    if let Some(rows) = &mut self.table {
                        rows.push(Vec::new());
                    }
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::TableCell => {
                    let cell = std::mem::take(&mut self.cur);
                    if let Some(row) = self.table.as_mut().and_then(|rows| rows.last_mut()) {
                        row.push(cell);
                    }
                }
                TagEnd::Table => self.end_table(),
                TagEnd::Paragraph => {
                    self.end_line();
                    self.blank();
                }
                TagEnd::Heading(_) => {
                    self.heading = false;
                    self.end_line();
                    self.blank();
                }
                TagEnd::BlockQuote(_) => {
                    self.quote = self.quote.saturating_sub(1);
                    self.blank();
                }
                TagEnd::CodeBlock => {
                    self.code_block = false;
                    self.end_line();
                    self.blank();
                }
                TagEnd::List(_) => {
                    self.list_stack.pop();
                    // Only the outermost list is followed by a separator; a
                    // nested one runs straight on into its parent's next item.
                    if self.list_stack.is_empty() {
                        self.blank();
                    }
                }
                TagEnd::Item => self.end_line(),
                TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
                TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
                TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
                TagEnd::Link => self.end_link(),
                _ => {}
            },
            Event::Text(t) => self.push_text(&t),
            Event::Code(t) => {
                self.cur
                    .push(Span::styled(t.to_string(), Style::default().fg(CODE)));
            }
            Event::SoftBreak => {
                if !self.code_block {
                    self.cur.push(Span::raw(" "));
                }
            }
            Event::HardBreak => self.end_line(),
            Event::TaskListMarker(checked) => {
                let mark = if checked { "[x] " } else { "[ ] " };
                self.cur.push(Span::styled(mark, Style::default().fg(DIM)));
            }
            Event::Rule => {
                self.end_line();
                let n = self.width.clamp(4, 80) as usize;
                self.lines.push(Line::from(Span::styled(
                    "─".repeat(n),
                    Style::default().fg(DIM),
                )));
                self.blank();
            }
            // HTML and other inline/block events: render their text verbatim.
            Event::Html(t) | Event::InlineHtml(t) => self.push_text(&t),
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.end_line();
        while self.lines.last().map(|l| l.width() == 0).unwrap_or(false) {
            self.lines.pop();
        }
        self.lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn code_block_preserves_blank_lines() {
        // A blank line inside a fenced block is real code layout; dropping it
        // fuses the surrounding lines together.
        let lines = render("```\nfn a() {}\n\nfn b() {}\n```", 80);
        let rendered = texts(&lines);
        let a = rendered
            .iter()
            .position(|l| l.contains("fn a()"))
            .expect("first code line rendered");
        let b = rendered
            .iter()
            .position(|l| l.contains("fn b()"))
            .expect("second code line rendered");
        assert_eq!(b - a, 2, "blank separator preserved: {rendered:?}");
        assert!(rendered[a + 1].trim().is_empty());
    }

    /// A nested list starts on its own line under its parent item. The first
    /// nested marker used to join the parent's text (`• a  • nested b`).
    #[test]
    fn nested_lists_start_on_their_own_line() {
        assert_eq!(
            texts(&render("- a\n  - nested b\n  - nested c\n- d", 80)),
            ["• a", "  • nested b", "  • nested c", "• d"]
        );
        assert_eq!(
            texts(&render("1. one\n   - sub\n2. two\n\nafter", 80)),
            ["1. one", "  • sub", "2. two", "", "after"]
        );
    }

    /// Each table row is its own line, with the columns aligned and a rule
    /// under the header. Without `ENABLE_TABLES` the whole table was one
    /// paragraph line of pipes.
    #[test]
    fn tables_render_one_line_per_row() {
        let md = "| name | n |\n|---|---|\n| alpha | 1 |\n| b | 22 |\n\nafter";
        assert_eq!(
            texts(&render(md, 80)),
            [
                "name  │ n",
                "──────┼───",
                "alpha │ 1",
                "b     │ 22",
                "",
                "after"
            ]
        );
    }

    /// A link shows where it goes, unless its text already is the URL.
    #[test]
    fn links_show_their_destination() {
        assert_eq!(
            texts(&render("see [the docs](https://example.com/d) now", 80)),
            ["see the docs (https://example.com/d) now"]
        );
        assert_eq!(
            texts(&render("<https://example.com> and <a@b.co>", 80)),
            ["https://example.com and a@b.co"]
        );
    }
}
