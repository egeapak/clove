//! Rendering. `render` is the only public entry; everything else is
//! `pub(crate)` and split per-component / per-page.
//!
//! The body layout adapts to the terminal shape (see [`pick_layout`]):
//! side-by-side when wide, list-over-detail when tall-and-medium, and a single
//! focused pane when narrow or short. List rows and the status/tab bars also
//! degrade gracefully as space shrinks.

mod detail;
mod filter_menu;
mod help;
mod list;
mod status;
mod style;
mod tabs;
mod util;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, Focus, Mode};

use detail::render_detail;
use filter_menu::render_filter_menu;
use help::render_help;
use list::render_list;
use status::render_status;
use tabs::render_tabs;

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
