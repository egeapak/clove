//! Add/edit form overlay rendering.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};
use ratatui::Frame;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Field, FormMode};

use super::style::{ACCENT, DIM, LABEL};
use super::util::centered_fixed;

/// The width of a field label column: pointer (1) + a 9-wide right-justified
/// `Name:` + a trailing space. All field names are ≤ 8 chars, so `{:>9}` is
/// always exactly 9 columns and this constant holds.
const LABEL_COLS: u16 = 11;

/// The add/edit form: one row per field (enum fields show `‹ value ›`), an error
/// line, and a key hint. The focused text field's caret is shown via the
/// terminal's **hardware cursor** (placed on the caret cell); for cursor-less
/// backends — the snapshot tests + PNG screenshots — `app.caret_glyph` also draws
/// an in-buffer `▏` glyph at the same cell. Live terminals leave the glyph off so
/// the two don't double up.
pub(crate) fn render_form(f: &mut Frame, app: &App, area: Rect) {
    let form = &app.form;
    let mut lines: Vec<Line> = Vec::new();
    let mut cursor_line: u16 = 0;
    // The caret's absolute (pre-scroll) position within the content: `(line, col)`.
    // `None` while an enum field is focused (no text caret).
    let mut caret: Option<(u16, u16)> = None;

    for (i, &field) in form.fields.iter().enumerate() {
        let focused = i == form.focus;
        if focused {
            cursor_line = lines.len() as u16;
        }
        let pointer = if focused { "▌" } else { " " };
        let label = format!("{pointer}{:>9} ", format!("{}:", field.label()));
        let label_style = if focused {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(LABEL)
        };

        if field == Field::Body {
            lines.push(Line::from(Span::styled(label, label_style)));
            // Body content below the label, indented; the caret is drawn inline at
            // the cursor position (it may land mid-text or on its own new line).
            if focused {
                let before = &form.body[..char_byte(&form.body, form.cursor)];
                let lines_before = before.matches('\n').count() as u16;
                // Display columns (not chars) so wide chars don't offset the caret.
                let col = display_width(before.rsplit('\n').next().unwrap_or(""));
                caret = Some((cursor_line + 1 + lines_before, 2 + col));
            }
            let body = if focused && app.caret_glyph {
                with_caret(&form.body, form.cursor)
            } else {
                form.body.clone()
            };
            let mut shown: Vec<&str> = body.split('\n').collect();
            // Drop a trailing empty line only when unfocused — when focused the
            // caret may sit on that line, so both glyph and live modes must render
            // it (otherwise the hardware cursor lands a row past the body).
            if !focused && shown.last() == Some(&"") && shown.len() > 1 {
                shown.pop();
            }
            // Two spans per line (indent, text) so a horizontal scroll can shift
            // the text and keep the indent, like the label on a field row.
            for raw in &shown {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled((*raw).to_owned(), Style::default().fg(Color::Reset)),
                ]));
            }
            continue;
        }

        let value: String = if field.is_enum() {
            let arrows = if focused { ("‹ ", " ›") } else { ("", "") };
            format!("{}{}{}", arrows.0, form.enum_value(field), arrows.1)
        } else if focused {
            // Caret column in *display* columns from the value's start, so wide /
            // zero-width chars before the caret don't offset it from the glyph.
            let prefix_w = prefix_width(&text_value(form, field), form.cursor);
            caret = Some((cursor_line, LABEL_COLS + prefix_w));
            if app.caret_glyph {
                with_caret(&text_value(form, field), form.cursor)
            } else {
                text_value(form, field)
            }
        } else {
            text_value(form, field)
        };
        let value_style = if focused {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Reset)
        };
        lines.push(Line::from(vec![
            Span::styled(label, label_style),
            Span::styled(value, value_style),
        ]));
    }

    // The error and the key hint live in a footer below the field rows. The
    // footer does not scroll, so a submit error stays on screen however long
    // the body is. (Both used to be appended to the scrolling rows, where a body
    // taller than the popup pushed the error out of view and a failed Ctrl-S
    // looked like a no-op.)
    let hint = Line::from(Span::styled(
        "Ctrl-S save · Esc cancel · Tab/↑↓ field · ←→ move/change",
        Style::default().fg(DIM),
    ));

    let title = match form.mode {
        FormMode::New => " New item ".to_owned(),
        FormMode::Edit => format!(
            " Edit {} ",
            form.edit_id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_default()
        ),
    };

    let field_rows = lines.len() as u16;
    let full_screen = area.width < 56 || area.height < 16;
    let popup_w = if full_screen {
        area.width
    } else {
        64.min(area.width.saturating_sub(2))
    };
    let inner_w = popup_w.saturating_sub(4); // 2 borders + 2 padding cols
    let error_rows = form
        .error
        .as_deref()
        .map_or(0, |err| wrap_error(err, inner_w).len() as u16);
    // Field rows, then (spacer + error), then spacer + hint.
    let wanted = field_rows + if error_rows > 0 { error_rows + 1 } else { 0 } + 2;
    let popup = if full_screen {
        area
    } else {
        let h = (wanted + 2).min(area.height.saturating_sub(2)).min(30);
        centered_fixed(area, popup_w, h)
    };
    let inner_h = popup.height.saturating_sub(2);

    let footer = footer_lines(form.error.as_deref(), hint, inner_w, inner_h);
    let footer_h = footer.len() as u16;
    // The field rows get what the footer leaves; the footer sits right under
    // them (not pinned to the popup's bottom edge) when they are short.
    let fields_h = field_rows.min(inner_h.saturating_sub(footer_h));

    // Vertical scroll so the caret (or, for enum fields, the focused row) stays
    // visible.
    let focus_line = caret.map(|(l, _)| l).unwrap_or(cursor_line);
    let scroll = focus_line.saturating_sub(fields_h.saturating_sub(1));
    // Horizontal scroll so a caret past the right edge of a long value is
    // brought back into view instead of vanishing (blind typing). Only the
    // caret line's *value* scrolls: the labels of every row (and the caret
    // row's own label or body indent) stay in place.
    let hscroll = match caret {
        Some((line, col)) => {
            let hscroll = col.saturating_sub(inner_w.saturating_sub(1));
            if hscroll > 0 {
                if let Some(l) = lines.get_mut(line as usize) {
                    scroll_value(l, hscroll);
                }
            }
            hscroll
        }
        None => 0,
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .padding(Padding::new(1, 1, 0, 0))
        .title(title)
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(popup);
    f.render_widget(Clear, popup);
    f.render_widget(block, popup);
    let fields_area = Rect {
        height: fields_h.min(inner.height),
        ..inner
    };
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), fields_area);
    let footer_area = Rect {
        y: inner.y + fields_area.height,
        height: footer_h.min(inner.height - fields_area.height),
        ..inner
    };
    f.render_widget(Paragraph::new(footer), footer_area);

    // Place the terminal's hardware cursor on the caret cell (coincides with the
    // glyph) when it's within the visible, scrolled content area — so real
    // terminals show a native blinking cursor, while the glyph keeps the caret
    // visible in the (cursor-less) snapshot/PNG tooling. `col` is a display
    // column, so wide/zero-width chars before the caret keep cursor and glyph
    // aligned; `hscroll` maps it back to a screen column.
    if let Some((line, col)) = caret {
        let inner_x = popup.x + 2; // left border + left padding
        let inner_y = popup.y + 1; // top border (no top padding)
        let vis_col = col.saturating_sub(hscroll);
        if line >= scroll && line < scroll + fields_h && vis_col < inner_w {
            f.set_cursor_position((inner_x + vis_col, inner_y + (line - scroll)));
        }
    }
}

/// The most rows a submit error may take in the footer; a longer one ends in `…`.
const ERROR_ROWS: u16 = 2;

/// The form's fixed footer: the submit error (when there is one), then the key
/// hint, each under a blank spacer row. The error is hard-wrapped to `width`
/// and capped at [`ERROR_ROWS`] rows. When the popup is too short for all of it
/// plus one field row, the spacers go first and then the hint: the error is the
/// last thing dropped.
fn footer_lines<'a>(error: Option<&str>, hint: Line<'a>, width: u16, height: u16) -> Vec<Line<'a>> {
    let style = Style::default().fg(Color::Red);
    let error_rows: Vec<Line> = error
        .map(|err| wrap_error(err, width))
        .unwrap_or_default()
        .into_iter()
        .map(|row| Line::from(Span::styled(row, style)))
        .collect();

    let budget = height.saturating_sub(1) as usize; // leave one row for the fields
    let has_error = !error_rows.is_empty();
    let full = error_rows.len() + if has_error { 3 } else { 2 };
    let mut out: Vec<Line> = Vec::new();
    if full <= budget {
        if has_error {
            out.push(Line::raw(""));
            out.extend(error_rows);
        }
        out.push(Line::raw(""));
        out.push(hint);
    } else if error_rows.len() < budget {
        out.extend(error_rows);
        out.push(hint);
    } else {
        out.extend(error_rows.into_iter().take(budget));
    }
    out
}

/// Hard-wrap an error message to `width` columns (after a two-column indent),
/// at most [`ERROR_ROWS`] rows; what does not fit is cut with `…`.
fn wrap_error(err: &str, width: u16) -> Vec<String> {
    let cols = (width as usize).saturating_sub(2).max(1);
    let flat = err.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    let mut used = 0usize;
    for g in flat.graphemes(true) {
        let w = g.width();
        if used + w > cols {
            rows.push(std::mem::take(&mut row));
            used = 0;
        }
        row.push_str(g);
        used += w;
    }
    if !row.is_empty() {
        rows.push(row);
    }
    if rows.len() > ERROR_ROWS as usize {
        rows.truncate(ERROR_ROWS as usize);
        if let Some(last) = rows.last_mut() {
            // Make room for the ellipsis by dropping the last grapheme.
            if let Some((b, _)) = last.grapheme_indices(true).next_back() {
                last.truncate(b);
            }
            last.push('…');
        }
    }
    rows.into_iter().map(|r| format!("  {r}")).collect()
}

/// Scroll the value of a caret line `cols` display columns to the left. The
/// line is `[prefix, value]` (a field label or the body indent, then the text);
/// the prefix is kept. The first visible column of the value becomes `…` so the
/// hidden start of the value is marked. Every other column keeps its place, so
/// the caret lands at screen column `col - cols`.
fn scroll_value(line: &mut Line, cols: u16) {
    let Some(span) = line.spans.get_mut(1) else {
        return;
    };
    // Hide `cols + 1` columns: `cols` scroll off, one more makes room for `…`.
    // Step by grapheme, as ratatui renders, so a cut never splits a cluster
    // (an emoji and its variation selector, a base and its combining mark).
    let hide = cols as usize + 1;
    let mut hidden = 0usize;
    let mut rest = "";
    for (b, g) in span.content.grapheme_indices(true) {
        if hidden >= hide {
            rest = &span.content[b..];
            break;
        }
        hidden += g.width();
    }
    // A wide grapheme that straddles the cut is hidden whole; pad its visible
    // half with a space so the columns after it stay aligned.
    let pad = " ".repeat(hidden.saturating_sub(hide));
    span.content = format!("…{pad}{rest}").into();
}

/// The display-column width of the first `chars` characters of `s` (the caret's
/// column offset from the value's start).
fn prefix_width(s: &str, chars: usize) -> u16 {
    display_width(&s[..char_byte(s, chars)])
}

/// The display-column width of `s` as ratatui renders it: the sum of each
/// grapheme's width. The caret column, the horizontal cut, and the rendered
/// cells all use this one measure, so the hardware cursor and the caret glyph
/// stay on the same cell (`❤\u{FE0F}` is one 2-column grapheme, not 1 + 0).
fn display_width(s: &str) -> u16 {
    s.graphemes(true).map(UnicodeWidthStr::width).sum::<usize>() as u16
}

/// The byte offset of char index `idx` in `s` (clamped to `s.len()`).
fn char_byte(s: &str, idx: usize) -> usize {
    s.char_indices().nth(idx).map(|(b, _)| b).unwrap_or(s.len())
}

/// Insert the caret glyph `▏` at char index `cursor` (clamped), so a moved
/// cursor renders mid-text. At end-of-string this appends the caret.
fn with_caret(s: &str, cursor: usize) -> String {
    let byte = char_byte(s, cursor);
    let mut out = String::with_capacity(s.len() + 3);
    out.push_str(&s[..byte]);
    out.push('▏');
    out.push_str(&s[byte..]);
    out
}

/// The current text buffer for a non-enum field.
fn text_value(form: &crate::app::FormState, field: Field) -> String {
    match field {
        Field::Title => form.title.clone(),
        Field::Assignee => form.assignee.clone(),
        Field::Labels => form.labels.clone(),
        Field::Parent => form.parent.clone(),
        Field::Deps => form.deps.clone(),
        _ => String::new(),
    }
}
