//! `clove-tui` — a read-only terminal UI for browsing clove work items.
//!
//! Launched by `clove tui`. It scans the file store (the source of truth) and
//! presents a master-detail browser: an All / Ready / Blocked item list on the
//! left and a per-item overview / dependency-tree / comments pane on the right.
//! It never mutates the store; refresh (`r`) re-scans from disk.

mod app;
mod markdown;
mod ui;

#[cfg(test)]
mod snapshot;

use anyhow::Result;
use clove_core::ItemStore;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use app::{App, DetailTab, Mode, Tab};

/// Run the TUI against a file store, blocking until the user quits.
///
/// Sets up the alternate screen + raw mode (and a panic hook that restores the
/// terminal) via [`ratatui::init`], and always restores on exit.
pub fn run(store: ItemStore) -> Result<()> {
    let mut app = App::new(store);
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    while !app.should_quit {
        terminal.draw(|f| ui::render(f, app))?;

        // Read-only browser: block on input — no animation to drive.
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        handle_key(app, key.code, key.modifiers);
    }
    Ok(())
}

fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    // Ctrl-C always quits, regardless of mode.
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.should_quit = true;
        return;
    }

    if app.mode == Mode::Search {
        match code {
            KeyCode::Char(c) => app.push_search(c),
            KeyCode::Backspace => app.pop_search(),
            KeyCode::Enter => app.commit_search(),
            KeyCode::Esc => app.cancel_search(),
            _ => {}
        }
        return;
    }

    // Filter menu owns the keyspace while open.
    if app.mode == Mode::Filter {
        match code {
            KeyCode::Down | KeyCode::Char('j') => app.filter_move(1),
            KeyCode::Up | KeyCode::Char('k') => app.filter_move(-1),
            KeyCode::Char(' ') | KeyCode::Enter | KeyCode::Right | KeyCode::Left => {
                app.filter_toggle()
            }
            KeyCode::Char('x') => app.clear_filters(),
            KeyCode::Esc | KeyCode::Char('f') | KeyCode::Char('q') => app.exit_filter(),
            _ => {}
        }
        return;
    }

    // Help overlay swallows everything except its dismiss keys.
    if app.show_help {
        if matches!(code, KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')) {
            app.show_help = false;
        }
        return;
    }

    match code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Esc => {
            // Esc unwinds the lightest active state: clear search, else return
            // focus to the list (matters in the single-pane narrow layout).
            if !app.search.is_empty() {
                app.cancel_search();
            } else {
                app.focus_list();
            }
        }
        KeyCode::Char('?') => app.show_help = true,

        KeyCode::Down | KeyCode::Char('j') => app.select_next(),
        KeyCode::Up | KeyCode::Char('k') => app.select_prev(),
        KeyCode::Char('g') | KeyCode::Home => app.select_first(),
        KeyCode::Char('G') | KeyCode::End => app.select_last(),

        KeyCode::Tab => app.next_tab(),
        KeyCode::Char('1') => app.set_tab(Tab::All),
        KeyCode::Char('2') => app.set_tab(Tab::Ready),
        KeyCode::Char('3') => app.set_tab(Tab::Blocked),

        KeyCode::Char('o') => app.set_detail_tab(DetailTab::Overview),
        KeyCode::Char('t') => app.set_detail_tab(DetailTab::Tree),
        KeyCode::Char('c') => app.set_detail_tab(DetailTab::Comments),

        // Pane focus (drives which pane shows in the narrow single-pane layout).
        KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => app.focus_detail(),
        KeyCode::Left | KeyCode::Char('h') => app.focus_list(),

        KeyCode::PageDown => app.scroll_detail_down(),
        KeyCode::PageUp => app.scroll_detail_up(),

        // Sort + filter.
        KeyCode::Char('s') => app.cycle_sort_field(),
        KeyCode::Char('S') => app.toggle_sort_dir(),
        KeyCode::Char('f') => app.start_filter(),
        KeyCode::Char('x') => app.clear_filters(),

        KeyCode::Char('/') => app.start_search(),
        KeyCode::Char('r') => app.refresh(),

        _ => {}
    }
}
