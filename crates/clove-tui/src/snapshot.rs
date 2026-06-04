//! Render snapshot tests (insta) validating the TUI across states and terminal
//! shapes. Every state is captured at three shapes so the adaptive layout is
//! exercised: portrait (40×48 → single pane), landscape (120×18 → side-by-side,
//! compact tab bar), and square (60×60 → stacked).
//!
//! Snapshots are taken from a deterministic fixture (fixed ids, timestamps, and
//! comments) and rendered to a [`TestBackend`]; the buffer is flattened to a
//! plain text grid (no styling) so snapshots stay font/colour-independent.

use camino::Utf8PathBuf;
use chrono::{DateTime, Utc};
use clove_core::comments::add_comment_at;
use clove_core::write::write_item_file;
use clove_core::{CloveId, Item, ItemFrontmatter, ItemStatus, ItemStore, ItemType, Priority};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

use crate::app::App;

/// The three terminal shapes every state is snapshotted at.
const SHAPES: [(&str, u16, u16); 3] = [
    ("portrait", 40, 48),
    ("landscape", 120, 18),
    ("square", 60, 60),
];

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().expect("valid RFC3339")
}

fn cid(s: &str) -> CloveId {
    CloveId::new(s).expect("valid id")
}

#[allow(clippy::too_many_arguments)]
fn put(
    store: &ItemStore,
    id: &str,
    title: &str,
    item_type: ItemType,
    priority: u8,
    status: ItemStatus,
    closed: Option<&str>,
    assignee: Option<&str>,
    parent: Option<&str>,
    labels: &[&str],
    deps: &[&str],
    relates: &[&str],
    body: &str,
) {
    // Distinct, non-monotonic `updated` per item so sort-by-updated visibly
    // reorders (and differs from id order). Derived from the id's last digit.
    let day = match id.chars().last().unwrap_or('1') {
        '1' => 10,
        '2' => 20,
        '3' => 12,
        '4' => 5,
        '5' => 18,
        '6' => 8,
        '7' => 25,
        _ => 1,
    };
    let id = cid(id);
    let fm = ItemFrontmatter {
        schema: 1,
        id: id.clone(),
        title: title.to_owned(),
        status,
        item_type,
        priority: Priority(priority),
        created: ts("2026-01-01T09:00:00Z"),
        updated: ts(&format!("2026-01-{day:02}T11:30:00Z")),
        closed: closed.map(ts),
        assignee: assignee.map(str::to_owned),
        parent: parent.map(cid),
        labels: labels.iter().map(|s| s.to_string()).collect(),
        deps: deps.iter().map(|s| cid(s)).collect(),
        relates: relates.iter().map(|s| cid(s)).collect(),
        duplicates: Vec::new(),
        supersedes: Vec::new(),
        source_system: None,
        external_ref: None,
    };
    let item = Item {
        frontmatter: fm,
        body: body.to_owned(),
    };
    write_item_file(&item, &store.path_for(&id)).expect("write item");
}

/// A small, dependency-rich fixture:
/// schema(closed) ← API(in_progress, ready) ← tests(blocked); an epic with one
/// closed + one open child; a bug that relates to the tests. The API item
/// carries two comments.
fn fixture() -> (tempfile::TempDir, ItemStore) {
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
    std::fs::create_dir_all(root.join(".clove").join("issues")).unwrap();
    let store = ItemStore::new(root);

    put(
        &store,
        "proj-00000001",
        "Set up database schema",
        ItemType::Chore,
        1,
        ItemStatus::Closed,
        Some("2026-02-01T12:00:00Z"),
        Some("ada"),
        None,
        &["area:db", "backend"],
        &[],
        &[],
        "Provision Postgres and run the initial migrations.",
    );
    put(
        &store,
        "proj-00000002",
        "Build REST API",
        ItemType::Feature,
        0,
        ItemStatus::InProgress,
        None,
        Some("grace"),
        None,
        &["area:api", "backend"],
        &["proj-00000001"],
        &[],
        "## Goals\n\nBuild a **REST API** with `axum`; see `docs/api.md`.\n\n- CRUD endpoints\n- Auth middleware\n\n### Steps\n\n1. Define routes\n2. Wire handlers\n\n> Blocked on the schema work until it lands.\n\n---\n\nDone when the integration tests pass.",
    );
    put(
        &store,
        "proj-00000003",
        "Write integration tests",
        ItemType::Feature,
        2,
        ItemStatus::Open,
        None,
        None,
        None,
        &["area:qa"],
        &["proj-00000002"],
        &[],
        "End-to-end coverage of the public API surface.",
    );
    put(
        &store,
        "proj-00000004",
        "Ship v1 release",
        ItemType::Epic,
        2,
        ItemStatus::Open,
        None,
        None,
        None,
        &["milestone"],
        &[],
        &[],
        "Tracking epic for the v1 launch.",
    );
    put(
        &store,
        "proj-00000005",
        "Frontend dashboard",
        ItemType::Feature,
        3,
        ItemStatus::Open,
        None,
        Some("ada"),
        Some("proj-00000004"),
        &["area:ui"],
        &[],
        &[],
        "React dashboard consuming the API.",
    );
    put(
        &store,
        "proj-00000006",
        "Docs site",
        ItemType::Docs,
        4,
        ItemStatus::Closed,
        Some("2026-02-02T09:00:00Z"),
        None,
        Some("proj-00000004"),
        &[],
        &[],
        &[],
        "Static documentation site.",
    );
    put(
        &store,
        "proj-00000007",
        "Investigate flaky CI",
        ItemType::Bug,
        1,
        ItemStatus::Open,
        None,
        Some("grace"),
        None,
        &["area:ci"],
        &[],
        &["proj-00000003"],
        "Integration tests time out intermittently on CI.",
    );

    let issues = store.issues_dir();
    let api = cid("proj-00000002");
    add_comment_at(
        issues,
        &api,
        "grace@example.com",
        "Started on the CRUD layer.",
        ts("2026-01-05T10:00:00Z"),
    )
    .unwrap();
    add_comment_at(
        issues,
        &api,
        "ada@example.com",
        "Schema is ready — you're unblocked.",
        ts("2026-01-06T14:30:00Z"),
    )
    .unwrap();

    (dir, store)
}

fn app() -> App {
    let (dir, store) = fixture();
    // Keep the temp dir alive for the lifetime of the app by leaking it; the
    // process is a short-lived test binary.
    std::mem::forget(dir);
    App::new(store)
}

/// Flatten the rendered terminal buffer to a trimmed text grid.
fn render_to_string(app: &mut App, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| crate::ui::render(f, app)).unwrap();
    let buf = terminal.backend().buffer();
    let area = buf.area;
    let mut out = String::new();
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                line.push_str(cell.symbol());
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Snapshot `app` at all three shapes under `name`.
fn snap(name: &str, app: &mut App) {
    for (shape, w, h) in SHAPES {
        let rendered = render_to_string(app, w, h);
        insta::assert_snapshot!(format!("{name}_{shape}"), rendered);
    }
}

#[test]
fn overview() {
    // Default: All tab, first item (the ready API task) selected, Overview.
    let mut app = app();
    snap("overview", &mut app);
}

#[test]
fn blocked_tab() {
    let mut app = app();
    app.set_tab(crate::app::Tab::Blocked);
    snap("blocked_tab", &mut app);
}

#[test]
fn dep_tree() {
    let mut app = app();
    app.set_detail_tab(crate::app::DetailTab::Tree);
    snap("dep_tree", &mut app);
}

#[test]
fn comments() {
    let mut app = app();
    app.set_detail_tab(crate::app::DetailTab::Comments);
    snap("comments", &mut app);
}

#[test]
fn search_active() {
    let mut app = app();
    app.start_search();
    for c in "api".chars() {
        app.push_search(c);
    }
    snap("search_active", &mut app);
}

#[test]
fn help_overlay() {
    let mut app = app();
    app.show_help = true;
    snap("help_overlay", &mut app);
}

#[test]
fn detail_focused() {
    // In the narrow (portrait) layout this shows the detail pane instead of the
    // list; in wide/stacked layouts both panes remain visible.
    let mut app = app();
    app.focus_detail();
    snap("detail_focused", &mut app);
}

#[test]
fn empty_repo() {
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
    std::fs::create_dir_all(root.join(".clove").join("issues")).unwrap();
    let mut app = App::new(ItemStore::new(root));
    snap("empty_repo", &mut app);
}

#[test]
fn filter_menu() {
    // Open the menu and toggle a single-valued (status:open) and a multi-valued
    // (type:feature) facet so the radio + checkbox states both show.
    let mut app = app();
    app.start_filter();
    app.filter_toggle(); // cursor at first row = status:open
    app.filter_cursor = 4; // a type row (after 3 status rows + header offset is by index)
    app.filter_toggle();
    snap("filter_menu", &mut app);
}

#[test]
fn filtered_by_type() {
    // Apply type=feature via the menu, then return to Browse to show the narrowed
    // list, the Items (N/M) title, and the filter chip in the status line.
    let mut app = app();
    app.start_filter();
    app.filter_cursor = 4; // type:feature row
    app.filter_toggle();
    app.exit_filter();
    snap("filtered_by_type", &mut app);
}

#[test]
fn sorted_by_updated() {
    let mut app = app();
    // rank → priority → created → updated
    for _ in 0..3 {
        app.cycle_sort_field();
    }
    snap("sorted_by_updated", &mut app);
}

#[test]
fn filtered_empty() {
    // status:closed AND type:bug matches nothing in the fixture — exercises the
    // empty-result escape hatch.
    let mut app = app();
    app.start_filter();
    app.filter_cursor = 2; // status:closed
    app.filter_toggle();
    app.filter_cursor = 3; // type:bug
    app.filter_toggle();
    app.exit_filter();
    snap("filtered_empty", &mut app);
}

// --- Overview edge cases: long title, long labels, scroll -----------------

fn mk_store() -> (tempfile::TempDir, ItemStore) {
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
    std::fs::create_dir_all(root.join(".clove").join("issues")).unwrap();
    (dir, ItemStore::new(root))
}

/// A focused fixture: a dependency target plus a feature item (`#2`, priority 0
/// so it's selected first) with the given title, labels, and body. When
/// `blocked`, the dep target is open so the item shows a blocker.
fn edge_app(title: &str, labels: &[&str], blocked: bool, body: &str) -> App {
    let (dir, store) = mk_store();
    let (dep_status, dep_closed) = if blocked {
        (ItemStatus::Open, None)
    } else {
        (ItemStatus::Closed, Some("2026-02-01T12:00:00Z"))
    };
    #[rustfmt::skip]
    put(&store, "proj-00000001", "Dependency target", ItemType::Chore, 2,
        dep_status, dep_closed, None, None, &[], &[], &[], "");
    #[rustfmt::skip]
    put(&store, "proj-00000002", title, ItemType::Feature, 0,
        ItemStatus::InProgress, None, Some("ada"), Some("proj-00000001"),
        labels, &["proj-00000001"], &["proj-00000001"], body);
    std::mem::forget(dir);
    App::new(store)
}

const LONG_TITLE: &str =
    "Implement end-to-end offline synchronization with conflict resolution and retry backoff";
const MANY_LABELS: &[&str] = &[
    "area:sync",
    "area:mobile",
    "backend",
    "frontend",
    "infra",
    "kind:regression",
    "needs-review",
    "priority:high",
    "team:platform",
    "flaky",
];

const SCROLL_BODY: &str = "## Overview\n\nA longer body so the detail scrolls.\n\n- point one\n- point two\n- point three\n\n## Details\n\nMore text to fill vertical space and exercise the scroll offset while the fixed header and pinned footer stay put.\n\n## Notes\n\nThe final paragraph.";

#[test]
fn overview_long_title() {
    // Wide: title truncated to fit beside the status. Narrow (focused portrait):
    // title wraps to multiple lines.
    let mut app = edge_app(LONG_TITLE, &["area:sync", "backend"], false, "");
    app.focus_detail();
    snap("overview_long_title", &mut app);
}

#[test]
fn overview_long_labels() {
    // Wide: footer labels truncate with `+N`. Narrow (focused portrait): inline
    // labels wrap.
    let mut app = edge_app("Short title", MANY_LABELS, false, "");
    app.focus_detail();
    snap("overview_long_labels", &mut app);
}

#[test]
fn overview_scroll() {
    // Scrolled detail: the body region scrolls while the wide layout's fixed
    // header and pinned footer stay put; narrow scrolls everything (no footer).
    let mut app = edge_app(LONG_TITLE, MANY_LABELS, true, SCROLL_BODY);
    app.detail_scroll = 6;
    let wide = render_to_string(&mut app, 120, 24);
    insta::assert_snapshot!("overview_scroll_wide", wide);

    app.detail_scroll = 3;
    let narrow = render_to_string(&mut app, 40, 18);
    insta::assert_snapshot!("overview_scroll_narrow", narrow);
}

// --- PNG screenshot generation (manual, #[ignore]) ------------------------
//
//   cargo test -p clove-tui generate_screenshots -- --ignored --nocapture
//
// Renders each screen's real cell buffer (colours + bold) to a PNG under
// docs/screenshots/ using a system monospace font with full glyph coverage
// (DejaVu Sans Mono preferred). Tooling, not a CI test.

mod shots {
    use super::*;
    use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
    use image::{Rgb, RgbImage};
    use ratatui::style::{Color, Modifier};

    const BG: [u8; 3] = [0x1d, 0x20, 0x27];
    const FG: [u8; 3] = [0xc8, 0xcc, 0xd4];

    /// Monospace font candidates (regular, bold), tried in order. DejaVu Sans
    /// Mono first for its broad box-drawing / geometric-shape coverage.
    const FONTS: &[(&str, &str)] = &[
        (
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf",
        ),
        (
            "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationMono-Bold.ttf",
        ),
    ];

    fn load_fonts() -> (FontVec, FontVec) {
        for (reg, bold) in FONTS {
            if let (Ok(r), Ok(b)) = (std::fs::read(reg), std::fs::read(bold)) {
                return (
                    FontVec::try_from_vec(r).expect("valid ttf"),
                    FontVec::try_from_vec(b).expect("valid ttf"),
                );
            }
        }
        panic!("no monospace font found among {FONTS:?}");
    }

    fn rgb(c: Color, default: [u8; 3]) -> [u8; 3] {
        match c {
            Color::Reset => default,
            Color::Black => [0x1d, 0x20, 0x27],
            Color::Red => [0xe0, 0x6c, 0x75],
            Color::Green => [0x98, 0xc3, 0x79],
            Color::Yellow => [0xe5, 0xc0, 0x7b],
            Color::Blue => [0x61, 0xaf, 0xef],
            Color::Magenta => [0xc6, 0x78, 0xdd],
            Color::Cyan => [0x56, 0xb6, 0xc2],
            Color::Gray => [0xab, 0xb2, 0xbf],
            Color::DarkGray => [0x5c, 0x63, 0x70],
            Color::LightRed => [0xff, 0x8b, 0x94],
            Color::LightGreen => [0xb5, 0xe8, 0x90],
            Color::LightYellow => [0xff, 0xe0, 0x82],
            Color::LightBlue => [0x8a, 0xc6, 0xff],
            Color::LightMagenta => [0xe0, 0x9c, 0xff],
            Color::LightCyan => [0x7f, 0xe0, 0xea],
            Color::White => [0xff, 0xff, 0xff],
            Color::Rgb(r, g, b) => [r, g, b],
            Color::Indexed(n) => indexed(n),
        }
    }

    /// The xterm 256-colour palette.
    fn indexed(n: u8) -> [u8; 3] {
        match n {
            0..=15 => {
                const C: [[u8; 3]; 16] = [
                    [0, 0, 0],
                    [0xcd, 0, 0],
                    [0, 0xcd, 0],
                    [0xcd, 0xcd, 0],
                    [0, 0, 0xee],
                    [0xcd, 0, 0xcd],
                    [0, 0xcd, 0xcd],
                    [0xe5, 0xe5, 0xe5],
                    [0x7f, 0x7f, 0x7f],
                    [0xff, 0, 0],
                    [0, 0xff, 0],
                    [0xff, 0xff, 0],
                    [0x5c, 0x5c, 0xff],
                    [0xff, 0, 0xff],
                    [0, 0xff, 0xff],
                    [0xff, 0xff, 0xff],
                ];
                C[n as usize]
            }
            16..=231 => {
                let n = n - 16;
                let steps = [0u8, 95, 135, 175, 215, 255];
                [
                    steps[(n / 36) as usize],
                    steps[((n / 6) % 6) as usize],
                    steps[(n % 6) as usize],
                ]
            }
            232..=255 => {
                let v = 8 + (n - 232) * 10;
                [v, v, v]
            }
        }
    }

    fn blend(fg: [u8; 3], bg: [u8; 3], t: f32) -> [u8; 3] {
        let m = |a: u8, b: u8| (a as f32 * t + b as f32 * (1.0 - t)).round() as u8;
        [m(fg[0], bg[0]), m(fg[1], bg[1]), m(fg[2], bg[2])]
    }

    /// Last-resort substitution for any glyph the chosen font lacks.
    fn subst(font: &FontVec, ch: char) -> char {
        if font.glyph_id(ch).0 != 0 {
            ch
        } else {
            match ch {
                '◐' => '◑',
                '✗' => '×',
                '●' => '*',
                '○' => 'o',
                '•' => '*',
                '▌' | '▏' => '|',
                _ => ch,
            }
        }
    }

    pub fn render_png(app: &mut App, w: u16, h: u16, path: &str) {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| crate::ui::render(f, app)).unwrap();
        let buf = term.backend().buffer().clone();

        let (reg, bold) = load_fonts();
        let scale = PxScale::from(22.0);
        let sf = reg.as_scaled(scale);
        let cell_w = sf.h_advance(reg.glyph_id('M')).round().max(1.0) as u32;
        let ascent = sf.ascent();
        let cell_h = (sf.ascent() - sf.descent() + sf.line_gap()).round() as u32;

        let mut img = RgbImage::from_pixel(cell_w * w as u32, cell_h * h as u32, Rgb(BG));
        for cy in 0..h {
            for cx in 0..w {
                let cell = buf.cell((cx, cy)).unwrap();
                let m = cell.modifier;
                let mut fg = rgb(cell.fg, FG);
                let mut bg = rgb(cell.bg, BG);
                if m.contains(Modifier::REVERSED) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if m.contains(Modifier::DIM) {
                    fg = blend(fg, bg, 0.5);
                }
                let (x0, y0) = (cx as u32 * cell_w, cy as u32 * cell_h);
                for yy in 0..cell_h {
                    for xx in 0..cell_w {
                        img.put_pixel(x0 + xx, y0 + yy, Rgb(bg));
                    }
                }
                let ch = cell.symbol().chars().next().unwrap_or(' ');
                if ch == ' ' {
                    continue;
                }
                let f = if m.contains(Modifier::BOLD) {
                    &bold
                } else {
                    &reg
                };
                let ch = subst(f, ch);
                let glyph = f
                    .glyph_id(ch)
                    .with_scale_and_position(scale, ab_glyph::point(x0 as f32, y0 as f32 + ascent));
                if let Some(o) = f.outline_glyph(glyph) {
                    let bb = o.px_bounds();
                    o.draw(|gx, gy, cov| {
                        let ix = bb.min.x as i32 + gx as i32;
                        let iy = bb.min.y as i32 + gy as i32;
                        if ix >= 0
                            && iy >= 0
                            && (ix as u32) < img.width()
                            && (iy as u32) < img.height()
                        {
                            let under = img.get_pixel(ix as u32, iy as u32).0;
                            img.put_pixel(ix as u32, iy as u32, Rgb(blend(fg, under, cov)));
                        }
                    });
                }
            }
        }
        img.save(path).unwrap();
    }
}

#[test]
#[ignore = "manual screenshot generation"]
fn generate_screenshots() {
    let out = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/screenshots");
    std::fs::create_dir_all(out).unwrap();
    let save = |name: &str, w: u16, h: u16, app: &mut App| {
        shots::render_png(app, w, h, &format!("{out}/{name}.png"))
    };

    let (ww, wh) = (120u16, 34u16);
    let (pw, ph) = (46u16, 40u16);

    {
        let mut a = app();
        save("01-overview", ww, wh, &mut a);
    }
    {
        let mut a = app();
        a.set_detail_tab(crate::app::DetailTab::Tree);
        save("02-dep-tree", ww, wh, &mut a);
    }
    {
        let mut a = app();
        a.set_detail_tab(crate::app::DetailTab::Comments);
        save("03-comments", ww, wh, &mut a);
    }
    {
        let mut a = app();
        a.set_tab(crate::app::Tab::Blocked);
        save("04-blocked", ww, wh, &mut a);
    }
    {
        let mut a = app();
        a.start_filter();
        a.filter_toggle();
        a.filter_cursor = 4;
        a.filter_toggle();
        save("05-filter-menu", 80, wh, &mut a);
    }
    {
        let mut a = app();
        a.start_filter();
        a.filter_cursor = 4;
        a.filter_toggle();
        a.exit_filter();
        save("06-filtered", ww, wh, &mut a);
    }
    {
        let mut a = app();
        for _ in 0..3 {
            a.cycle_sort_field();
        }
        save("07-sorted-updated", ww, wh, &mut a);
    }
    {
        let mut a = app();
        a.start_search();
        for c in "api".chars() {
            a.push_search(c);
        }
        save("08-search", ww, wh, &mut a);
    }
    {
        let mut a = app();
        a.show_help = true;
        save("09-help", ww, wh, &mut a);
    }
    {
        let mut a = app();
        a.focus_detail();
        save("10-portrait-detail", pw, ph, &mut a);
    }
    {
        let mut a = app();
        save("11-portrait-list", pw, ph, &mut a);
    }
}

// --- data-layer unit tests (rich fixture, no snapshots) -------------------

#[test]
fn filter_menu_lists_present_values() {
    // 3 statuses + 5 types + 5 priorities + 7 labels + 2 assignees = 22 rows.
    let app = app();
    assert_eq!(app.filter_menu.len(), 22);
}

#[test]
fn filter_single_type_narrows_and_clears() {
    let mut app = app();
    app.start_filter();
    app.filter_cursor = 4; // type:feature
    app.filter_toggle();
    app.exit_filter();
    // Three features: Build REST API, Write integration tests, Frontend dashboard.
    assert_eq!(app.visible_count(), 3);
    app.clear_filters();
    assert_eq!(app.visible_count(), 7);
}

#[test]
fn filter_multi_type_is_or() {
    let mut app = app();
    app.start_filter();
    app.filter_cursor = 3; // type:bug
    app.filter_toggle();
    app.filter_cursor = 4; // type:feature
    app.filter_toggle();
    // bug (1) OR feature (3) = 4 items.
    assert_eq!(app.visible_count(), 4);
}

#[test]
fn filter_across_facets_is_and() {
    let mut app = app();
    app.start_filter();
    app.filter_cursor = 4; // type:feature
    app.filter_toggle();
    // Find the priority p0 row dynamically (index depends on present facets).
    let p0 = app
        .filter_menu
        .iter()
        .position(|m| matches!(m.value, crate::app::MenuValue::Priority(0)))
        .unwrap();
    app.filter_cursor = p0;
    app.filter_toggle();
    // feature AND p0 → only "Build REST API".
    assert_eq!(app.visible_count(), 1);
    assert_eq!(app.selected_frontmatter().unwrap().title, "Build REST API");
}

#[test]
fn sort_by_id_orders_ascending() {
    let mut app = app();
    // rank → priority → created → updated → id
    for _ in 0..4 {
        app.cycle_sort_field();
    }
    assert_eq!(app.sort.field, crate::app::SortField::Id);
    let first = app.visible().next().unwrap();
    assert_eq!(first.id.as_str(), "proj-00000001");
}

#[test]
fn selection_survives_filtering() {
    let mut app = app();
    // Select the (feature) item that will survive a type:feature filter.
    app.select_first(); // Build REST API (p0 feature), the default first row
    let before = app.selected_frontmatter().unwrap().id.clone();
    app.start_filter();
    app.filter_cursor = 4; // type:feature (keeps it)
    app.filter_toggle();
    app.exit_filter();
    assert_eq!(app.selected_frontmatter().unwrap().id, before);
}
