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

/// Fixed "now" so relative timestamps are deterministic in snapshots.
const NOW: &str = "2026-02-15T12:00:00Z";

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
    let id = cid(id);
    let fm = ItemFrontmatter {
        schema: 1,
        id: id.clone(),
        title: title.to_owned(),
        status,
        item_type,
        priority: Priority(priority),
        created: ts("2026-01-01T09:00:00Z"),
        updated: ts("2026-01-03T11:30:00Z"),
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
    let mut app = App::new(store);
    app.now = ts(NOW); // pin relative timestamps
    app
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
    app.now = ts(NOW);
    snap("empty_repo", &mut app);
}
