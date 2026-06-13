//! Command-line surface (DESIGN.md §7.1, §7.2): global flags and the full M0
//! subcommand set.

use camino::Utf8PathBuf;
use clap::{Args, Parser, Subcommand, ValueEnum};
use clove_core::OutputFormat;

/// clove — a fast, git-native, dependency-aware work-item tracker.
#[derive(Debug, Parser)]
#[command(name = "clove", version, about, long_about = None)]
pub struct Cli {
    /// Output format.
    #[arg(short = 'f', long, global = true, value_parser = parse_format)]
    pub format: Option<OutputFormat>,

    /// Force a file scan even if an index is present.
    #[arg(long, global = true)]
    pub no_index: bool,

    /// Use the thorough per-file staleness check (stats every file) instead of
    /// the fast directory-level check, when reading via the index.
    #[arg(long, global = true)]
    pub deep: bool,

    /// Suppress informational stderr output.
    #[arg(long, global = true)]
    pub quiet: bool,

    /// Terminal color control.
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto)]
    pub color: ColorChoice,

    /// Override `.clove/` discovery with an explicit `.clove` directory.
    #[arg(long, global = true, value_name = "PATH")]
    pub clove_dir: Option<Utf8PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

/// Terminal color preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

/// The subcommand set.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Initialize a `.clove/` repository in the current directory.
    Init(InitArgs),
    /// Create a new item.
    New(NewArgs),
    /// Show one item.
    Show(ShowArgs),
    /// Edit an item (open `$EDITOR`, or `--field KEY=VALUE` for a field edit).
    Edit(EditArgs),
    /// Set one or more fields non-interactively (alias for `edit --field`).
    Set(SetArgs),
    /// Change an item's status (`open|in_progress|closed`).
    Status(StatusArgs),
    /// Mark an item in progress (alias for `status <id> in_progress`).
    Start(IdArg),
    /// Close an item (alias for `status <id> closed`).
    Close(IdArg),
    /// Add or remove a label.
    Label(LabelArgs),
    /// Set or clear the assignee.
    Assign(AssignArgs),
    /// Set the priority (0–4).
    Priority(PriorityArgs),
    /// Manage dependencies.
    Dep(DepArgs),
    /// List items that are ready to work on.
    Ready(FilterArgs),
    /// List items blocked by open dependencies.
    Blocked(FilterArgs),
    /// List items with optional filters.
    Ls(FilterArgs),
    /// Query items via a JSON filter (flag or stdin).
    Query(QueryArgs),
    /// Add a comment to an item.
    Comment(CommentArgs),
    /// List an item's comments.
    Comments(CommentsArgs),
    /// Full-text search.
    Search(SearchArgs),
    /// Show work-item analytics (counts, ready/blocked, epics, throughput).
    Stats(StatsArgs),
    /// Rebuild the SQLite index from the files.
    Reindex,
    /// Import items from another tracker (`tk|beads|github`).
    Import(ImportArgs),
    /// Export items to `json`, `jsonl`, or GitHub.
    Export(ExportArgs),
    /// Two-way sync items with a tracker (`github`).
    Sync(SyncArgs),
    /// Git 3-way merge driver for item files (`clove merge-driver %O %A %B %L`).
    MergeDriver(MergeDriverArgs),
    /// Generate an agent-facing usage document.
    AgentDoc(AgentDocArgs),
    /// Check the store for problems (optionally repair safe ones).
    Doctor(DoctorArgs),
    /// Control the optional background daemon (`start|stop|status`).
    Daemon(DaemonArgs),
    /// Browse items in an interactive, read-only terminal UI.
    Tui,
    /// Run the MCP server (stdio) so AI agents can use clove as native tools.
    Mcp,
    /// Serve the web UI (with a live file-watcher for real-time updates).
    Serve(ServeArgs),
    /// Print version and schema information.
    Version,
}

/// `clove serve` (DESIGN web UI / M4). Starts an HTTP server that serves the
/// embedded web UI and a JSON/WebSocket API for this repository.
#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Port to listen on.
    #[arg(long, default_value_t = 7373)]
    pub port: u16,

    /// Address to bind. Loopback only unless `--allow-non-loopback` is given.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    /// Open the served URL in the default browser.
    #[arg(long)]
    pub open: bool,

    /// Do not start the file-watcher (no real-time push from this process).
    #[arg(long)]
    pub no_watch: bool,

    /// Permit binding a non-loopback address (prints a security warning).
    #[arg(long)]
    pub allow_non_loopback: bool,
}

/// `clove daemon <start|stop|status>` (DESIGN §7.2, §8). The daemon is optional;
/// every read command works identically without it.
#[derive(Debug, Args)]
pub struct DaemonArgs {
    #[command(subcommand)]
    pub action: DaemonAction,
}

#[derive(Debug, Subcommand)]
pub enum DaemonAction {
    /// Start the daemon for this repository (spawns `cloved` detached).
    Start,
    /// Stop the running daemon.
    Stop,
    /// Show the running daemon's status.
    Status,
}

/// A bare `<id>` positional argument.
#[derive(Debug, Args)]
pub struct IdArg {
    /// The item id.
    pub id: String,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Override the generated id prefix.
    #[arg(long, value_name = "STR")]
    pub prefix: Option<String>,
    /// Also install the 3-way merge driver (`.gitattributes` + `.git/config`).
    #[arg(long)]
    pub merge_driver: bool,
}

#[derive(Debug, Args)]
pub struct NewArgs {
    /// The item title.
    pub title: String,
    /// Item type (bug|feature|chore|docs|epic). Defaults to the config default.
    #[arg(long = "type", value_name = "TYPE")]
    pub item_type: Option<String>,
    /// Priority 0 (highest) – 4. Defaults to 2.
    #[arg(short = 'p', long)]
    pub priority: Option<u8>,
    /// Add a label (repeatable).
    #[arg(short = 'l', long = "label", value_name = "LABEL")]
    pub labels: Vec<String>,
    /// Add a hard dependency (repeatable).
    #[arg(long = "dep", value_name = "ID")]
    pub deps: Vec<String>,
    /// Set the parent item.
    #[arg(long, value_name = "ID")]
    pub parent: Option<String>,
    /// Set the assignee.
    #[arg(short = 'a', long, value_name = "WHO")]
    pub assignee: Option<String>,
    /// Set the item body.
    #[arg(short = 'b', long, value_name = "TEXT")]
    pub body: Option<String>,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// The item id.
    pub id: String,
    /// Comma-separated field projection.
    #[arg(long, value_name = "LIST")]
    pub fields: Option<String>,
    /// Compute `ready`/`blocked_by` even for human output.
    #[arg(short = 'v', long)]
    pub verbose: bool,
}

#[derive(Debug, Args)]
pub struct EditArgs {
    /// The item id.
    pub id: String,
    /// A `KEY=VALUE` field edit (repeatable). If omitted, opens `$EDITOR`.
    #[arg(long = "field", value_name = "KEY=VALUE")]
    pub fields: Vec<String>,
}

#[derive(Debug, Args)]
pub struct SetArgs {
    /// The item id.
    pub id: String,
    /// One or more `KEY=VALUE` assignments.
    #[arg(value_name = "KEY=VALUE", required = true)]
    pub assignments: Vec<String>,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// The item id.
    pub id: String,
    /// The new status: `open`, `in_progress`, or `closed`.
    pub state: String,
}

#[derive(Debug, Args)]
pub struct LabelArgs {
    /// The item id.
    pub id: String,
    /// `add` or `rm`.
    pub action: String,
    /// The label value.
    pub label: String,
}

#[derive(Debug, Args)]
pub struct AssignArgs {
    /// The item id.
    pub id: String,
    /// The assignee (omit with `--clear` to unset).
    pub assignee: Option<String>,
    /// Clear the assignee.
    #[arg(long)]
    pub clear: bool,
}

#[derive(Debug, Args)]
pub struct PriorityArgs {
    /// The item id.
    pub id: String,
    /// Priority 0 (highest) – 4.
    pub priority: u8,
}

#[derive(Debug, Args)]
pub struct DepArgs {
    #[command(subcommand)]
    pub action: DepAction,
}

#[derive(Debug, Subcommand)]
pub enum DepAction {
    /// Add a hard dependency: `<id>` depends on `<dep-id>`.
    Add { id: String, dep_id: String },
    /// Remove a hard dependency.
    Rm { id: String, dep_id: String },
    /// Print the dependency tree rooted at `<id>`.
    Tree(DepTreeArgs),
    /// List dependency cycles.
    Cycle(DepCycleArgs),
}

#[derive(Debug, Args)]
pub struct DepTreeArgs {
    /// The root item id.
    pub id: String,
    /// Maximum depth (default 5).
    #[arg(long, default_value_t = 5)]
    pub depth: usize,
    /// Remove the depth limit.
    #[arg(long)]
    pub full: bool,
    /// Emit a flat array with a `depth` field instead of a nested tree.
    #[arg(long)]
    pub flat: bool,
}

#[derive(Debug, Args)]
pub struct DepCycleArgs {
    /// Exit 3 if any cycle is found.
    #[arg(long)]
    pub fail_on_cycle: bool,
}

/// Shared filter/pagination flags for `ls`, `ready`, `blocked`.
#[derive(Debug, Args, Default)]
pub struct FilterArgs {
    /// Filter by status (`open|in_progress|closed`).
    #[arg(long)]
    pub status: Option<String>,
    /// Filter by type.
    #[arg(long = "type", value_name = "TYPE")]
    pub item_type: Option<String>,
    /// Filter by label (canonicalized before matching).
    #[arg(long)]
    pub label: Option<String>,
    /// Filter by assignee.
    #[arg(long)]
    pub assignee: Option<String>,
    /// Filter by priority.
    #[arg(long)]
    pub priority: Option<u8>,
    /// Maximum number of results (default 100; use `--limit 0` for no limit).
    #[arg(long)]
    pub limit: Option<usize>,
    /// Skip this many results.
    #[arg(long)]
    pub offset: Option<usize>,
    /// Comma-separated field projection.
    #[arg(long, value_name = "LIST")]
    pub fields: Option<String>,
    /// Include items with dangling dependencies (ready/blocked).
    #[arg(long)]
    pub include_warnings: bool,
}

#[derive(Debug, Args)]
pub struct QueryArgs {
    /// A JSON filter object. If omitted and stdin is not a TTY, read it there.
    #[arg(long, value_name = "JSON")]
    pub filter: Option<String>,
    /// Comma-separated field projection.
    #[arg(long, value_name = "LIST")]
    pub fields: Option<String>,
    /// Maximum number of results (default 100; use `--limit 0` for no limit).
    #[arg(long)]
    pub limit: Option<usize>,
    /// Skip this many results.
    #[arg(long)]
    pub offset: Option<usize>,
}

#[derive(Debug, Args)]
pub struct CommentArgs {
    /// The item id.
    pub id: String,
    /// The comment body.
    pub message: String,
}

#[derive(Debug, Args)]
pub struct CommentsArgs {
    /// The item id.
    pub id: String,
    /// Show at most this many (most recent) comments.
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// The search text.
    pub text: String,
    /// Maximum number of results (default 100; use `--limit 0` for no limit).
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Debug, Args)]
pub struct StatsArgs {
    /// Cap the assignee/label breakdowns to the N highest counts (default 10;
    /// use `0` for no cap).
    #[arg(long, value_name = "N")]
    pub top: Option<usize>,
    /// Skip the per-epic completion rollup.
    #[arg(long)]
    pub no_epics: bool,
    /// Persist this report to the durable history in the index (`.clove/index.db`).
    #[arg(long)]
    pub snapshot: bool,
    /// Show the recorded snapshot history instead of a live report.
    #[arg(long)]
    pub history: bool,
    /// With `--history`: only snapshots at/after this RFC3339 timestamp.
    #[arg(long, value_name = "RFC3339")]
    pub since: Option<String>,
    /// With `--history`: show at most this many (most recent) snapshots.
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
}

#[derive(Debug, Args)]
pub struct AgentDocArgs {
    /// Write to a file instead of stdout.
    #[arg(long, value_name = "FILE")]
    pub out: Option<Utf8PathBuf>,
    /// Verify a file's embedded schema version matches this binary.
    #[arg(long)]
    pub check: bool,
    /// The file to check (with `--check`).
    #[arg(long, value_name = "PATH")]
    pub file: Option<Utf8PathBuf>,
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Apply safe repairs (labels, list order, orphaned comment dirs).
    #[arg(long)]
    pub fix: bool,
    /// Exit 4 while any unresolved error remains.
    #[arg(long)]
    pub strict: bool,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// The source tracker to import from.
    #[command(subcommand)]
    pub source: ImportSource,
}

/// The import source kind plus its source path/spec and shared flags.
///
/// Each variant carries a `src` (a directory or file path, or a `owner/repo`
/// spec for GitHub) and a `--dry-run` flag (plan only, no writes).
#[derive(Debug, Subcommand)]
pub enum ImportSource {
    /// Import a `tk` `.tickets/` directory (DESIGN §11.1).
    Tk {
        /// Path to the `.tickets/` directory.
        src: Utf8PathBuf,
        /// Plan only: report what would happen without writing any files.
        #[arg(long)]
        dry_run: bool,
    },
    /// Import a Beads `issues.jsonl` file (DESIGN §11.2).
    Beads {
        /// Path to the `issues.jsonl` file.
        src: Utf8PathBuf,
        /// Plan only: report what would happen without writing any files.
        #[arg(long)]
        dry_run: bool,
    },
    /// Import GitHub issues from `owner/repo` (DESIGN §11.3).
    Github {
        /// The `owner/repo` spec to fetch issues from.
        src: String,
        /// Plan only: report what would happen without writing any files.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// The export format.
    #[arg(value_enum, value_name = "FORMAT")]
    pub export_format: ExportFormat,
    /// For `github`: the `owner/repo` to push to (required for `github`).
    #[arg(value_name = "OWNER/REPO")]
    pub target: Option<String>,
    /// Write to a file instead of stdout.
    #[arg(long, value_name = "FILE")]
    pub out: Option<Utf8PathBuf>,
    /// For `github`: plan only, do not push anything.
    #[arg(long)]
    pub dry_run: bool,
}

/// `clove sync <github> <owner/repo>` (T-M06). One reconciled pull+push pass.
#[derive(Debug, Args)]
pub struct SyncArgs {
    /// The tracker to sync with. Only `github` is supported today.
    #[arg(value_enum, value_name = "TRACKER")]
    pub tracker: SyncTracker,
    /// The `owner/repo` to sync with.
    #[arg(value_name = "OWNER/REPO")]
    pub target: String,
    /// Plan only: report what would happen on both sides without writing anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Conflict policy for issues changed on both sides since the last sync:
    /// `newer` (default), `local`, `remote`, or `manual`.
    #[arg(long, value_name = "POLICY")]
    pub prefer: Option<String>,
    /// Skip syncing issue comments (faster: avoids one API call per issue).
    #[arg(long)]
    pub no_comments: bool,
}

/// The tracker a `clove sync` targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SyncTracker {
    /// GitHub Issues.
    Github,
}

/// The `clove export` output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExportFormat {
    /// A single JSON envelope with a `data` array of all items.
    Json,
    /// One item per line (NDJSON), Beads-isomorphic.
    Jsonl,
    /// Push to GitHub Issues via the REST API.
    Github,
}

#[derive(Debug, Args)]
pub struct MergeDriverArgs {
    /// The merge base (`%O`); may be absent for an add/add merge.
    pub ancestor: Utf8PathBuf,
    /// Our version (`%A`); the merged result is written back here.
    pub ours: Utf8PathBuf,
    /// Their version (`%B`).
    pub theirs: Utf8PathBuf,
    /// The conflict marker size (`%L`).
    pub marker_size: usize,
}

/// clap value-parser for [`OutputFormat`].
fn parse_format(raw: &str) -> Result<OutputFormat, String> {
    OutputFormat::parse(raw)
        .ok_or_else(|| format!("invalid format `{raw}` (expected human|json|jsonl)"))
}
