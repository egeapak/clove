//! A compact CommonMark → ratatui renderer for the item body.
//!
//! Built on `pulldown-cmark`, so inline emphasis/code, nested lists, code
//! blocks, block quotes, and rules parse correctly. Output is a `Vec<Line>`
//! styled for a terminal; soft line breaks become spaces so paragraphs reflow
//! under the `Paragraph` widget's word wrap, while hard breaks and block
//! boundaries start new lines.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::Indexed(240);
const CODE: Color = Color::Indexed(180);

/// Render Markdown `md` to styled lines, sizing rules to `width`.
pub fn render(md: &str, width: u16) -> Vec<Line<'static>> {
    let mut r = Renderer::new(width);
    let parser = Parser::new_ext(md, Options::ENABLE_STRIKETHROUGH);
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
    link: u32,
    code_block: bool,
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
        if self.link > 0 {
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

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { .. } => self.heading = true,
                Tag::BlockQuote(_) => self.quote += 1,
                Tag::CodeBlock(_) => {
                    self.end_line();
                    self.code_block = true;
                }
                Tag::List(start) => self.list_stack.push(start),
                Tag::Item => self.start_item(),
                Tag::Emphasis => self.italic += 1,
                Tag::Strong => self.bold += 1,
                Tag::Strikethrough => self.strike += 1,
                Tag::Link { .. } => self.link += 1,
                _ => {}
            },
            Event::End(tag) => match tag {
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
                    self.blank();
                }
                TagEnd::Item => self.end_line(),
                TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
                TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
                TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
                TagEnd::Link => self.link = self.link.saturating_sub(1),
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
}
