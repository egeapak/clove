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

    /// Force a file scan even if an index or daemon is present.
    ///
    /// A read-tier flag: it is accepted anywhere (before or after the
    /// subcommand) but only the commands that *choose a tier* act on it — ls,
    /// ready, blocked, query, stats, doctor, dep, serve — plus any plugin, which
    /// receives it as `$CLOVE_NO_INDEX` and decides for itself. It is inert
    /// everywhere else: `search` has a single file-scan tier by design, and the
    /// write and metadata commands (new, show, comments, version, …) never
    /// consult the index at all.
    #[arg(long, global = true)]
    pub no_index: bool,

    /// Use the thorough per-file staleness check (stats every file) instead of
    /// the fast directory-level check, when reading via the index.
    ///
    /// Same scope as `--no-index`: only the tier-choosing commands (ls, ready,
    /// blocked, query, stats, doctor, dep, serve) and plugins (`$CLOVE_DEEP`)
    /// act on it; it is accepted and inert elsewhere.
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
    /// Search titles, labels, and bodies for a case-insensitive substring.
    Search(SearchArgs),
    /// Show work-item analytics (counts, ready/blocked, epics, throughput).
    Stats(StatsArgs),
    /// Rebuild the SQLite index from the files.
    Reindex,
    /// Import items from a clove export (`json|jsonl`) or a tracker plugin.
    #[command(after_help = "\
Built-in providers: json, jsonl — clove's native restore, the inverse of \
`clove export json|jsonl`. `import json|jsonl <file> [--dry-run] [--overwrite]` \
restores items preserving their ids (an export → import round-trip is a \
backup/restore); existing ids are skipped unless --overwrite. Comments are not \
part of a clove export, so they are not restored. Any other provider is an \
external plugin: tk (a .tickets/ dir) needs clove-import-tk (cargo install \
clove-import-tk); beads (an issues.jsonl) needs clove-import-beads. A bidirectional \
plugin can also serve import: `import github` is served by clove-sync-github \
(pull-only view of the two-way sync).\n\
Note: clove global flags (--format, --color, --quiet, …) must come BEFORE the \
provider — everything after it is the provider's own arguments. \
e.g. `clove import --format json json a.json --overwrite`.")]
    Import(ImportArgs),
    /// Export items to `json` or `jsonl` (or via a tracker plugin).
    #[command(after_help = "\
Built-in providers: json, jsonl (clove's native item schema). Any other provider \
runs an external plugin — including a bidirectional one serving export: \
`export beads` is served by clove-import-beads (a beads-native issues.jsonl) and \
`export github` by clove-sync-github (push-only view of the two-way sync).\n\
Note: clove global flags (--format, --color, --quiet, …) must come BEFORE the \
provider — everything after it is the provider's own arguments. \
e.g. `clove export --format json json --out items.json`.")]
    Export(ExportArgs),
    /// Two-way sync items with a tracker (`github`).
    #[command(after_help = "\
github requires the clove-sync-github plugin (cargo install clove-sync-github). \
There are no built-in sync providers — every provider is an external \
clove-sync-<provider> plugin. The same clove-sync-github binary also serves the \
one-way `clove import github` (pull) and `clove export github` (push).\n\
Form: clove sync [--format json] github <owner/repo> [--dry-run] [--prefer P] \
[--no-comments].\n\
Note: clove global flags (--format, --color, --quiet, …) must come BEFORE the \
provider — everything after it is the provider's own arguments.")]
    Sync(SyncArgs),
    /// Git 3-way merge driver for item files (`clove merge-driver %O %A %B %L`).
    MergeDriver(MergeDriverArgs),
    /// Generate an agent-facing usage document.
    AgentDoc(AgentDocArgs),
    /// Register clove's MCP server with Claude Code and write CLOVE.md directives.
    Setup(SetupArgs),
    /// Check the store for problems (optionally repair safe ones).
    Doctor(DoctorArgs),
    /// Control the optional background daemon (`start|stop|status`).
    Daemon(DaemonArgs),
    /// Browse and edit items in an interactive terminal UI.
    Tui,
    /// Run the MCP server (stdio) so AI agents can use clove as native tools.
    Mcp,
    /// Serve the web UI (with a live file-watcher for real-time updates).
    Serve(ServeArgs),
    /// Print version and schema information.
    Version,
    /// Inspect installed subcommand plugins and discover published ones.
    Plugin(PluginArgs),
    /// Run an external subcommand plugin (`clove-<name>` on the search path).
    ///
    /// This catch-all fires only when the leading token matches no built-in, so a
    /// plugin can never shadow a real command (PLUGIN_SYSTEM.md §4.1). `argv[0]`
    /// is the subcommand name; the rest is forwarded to the plugin verbatim.
    #[command(external_subcommand)]
    External(Vec<String>),
}

/// `clove plugin <list|search>` — inspect installed plugins and discover
/// published ones.
#[derive(Debug, Args)]
pub struct PluginArgs {
    #[command(subcommand)]
    pub action: PluginAction,
}

#[derive(Debug, Subcommand)]
pub enum PluginAction {
    /// List installed plugins; with `--all`, also those published to crates.io.
    List(PluginListArgs),

    /// Search published plugins by name or description.
    ///
    /// Filters the discovered set locally: crates.io's own `?q=` is fuzzy
    /// full-text and returns nothing useful for a prefix like `clove-sync`.
    Search(PluginSearchArgs),

    /// Build and install a published plugin.
    ///
    /// This compiles and runs third-party code, so it always asks first. A
    /// non-interactive run refuses rather than proceeding — automation states
    /// its intent with `--yes`.
    Install(PluginInstallArgs),

    /// Remove a plugin clove installed. Needs no network.
    Uninstall(PluginUninstallArgs),

    /// Re-resolve installed plugins to their newest published version.
    Update(PluginUpdateArgs),
}

/// `clove plugin install <name> […]`.
#[derive(Debug, Args)]
pub struct PluginInstallArgs {
    /// The plugin to install: a provider (`gitlab`) or an exact crate name.
    ///
    /// A bare provider is resolved by constructing the candidate crate names;
    /// if several exist the command asks for the exact one rather than guessing
    /// which multiplexer wins.
    pub name: String,

    /// Skip the confirmation. Required for any non-interactive run.
    #[arg(long)]
    pub yes: bool,

    /// Reinstall even if the plugin is already present.
    #[arg(long)]
    pub force: bool,

    /// Treat an unverifiable `clove-plugin` dependency as fatal.
    ///
    /// Without this, a registry that cannot be reached downgrades the check to a
    /// warning shown in the prompt; with it, the install refuses instead.
    #[arg(long)]
    pub strict: bool,

    /// Install even when every published version is yanked.
    #[arg(long)]
    pub allow_yanked: bool,
}

/// `clove plugin uninstall <name>`.
#[derive(Debug, Args)]
pub struct PluginUninstallArgs {
    /// The plugin subcommand to remove, e.g. `sync-github`.
    pub name: String,
}

/// `clove plugin update [<name>] [--all]`.
#[derive(Debug, Args)]
pub struct PluginUpdateArgs {
    /// The plugin to update. Omit (or pass `--all`) to check every one.
    pub name: Option<String>,

    /// Check every clove-installed plugin.
    #[arg(long)]
    pub all: bool,

    /// Skip the confirmation. Required for any non-interactive run.
    #[arg(long)]
    pub yes: bool,
}

/// `clove plugin list [--all] [--refresh]`.
#[derive(Debug, Args)]
pub struct PluginListArgs {
    /// Also list plugins published to crates.io but not installed here.
    ///
    /// Without this flag the command is a pure filesystem walk and never
    /// touches the network.
    #[arg(long)]
    pub all: bool,

    /// Bypass the cached registry result and re-fetch (implies `--all`).
    #[arg(long)]
    pub refresh: bool,
}

/// `clove plugin search <query> [--refresh]`.
#[derive(Debug, Args)]
pub struct PluginSearchArgs {
    /// Text matched case-insensitively against plugin names and descriptions.
    pub query: String,

    /// Bypass the cached registry result and re-fetch.
    #[arg(long)]
    pub refresh: bool,
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
    #[arg(long, value_name = "STR", value_parser = parse_id_prefix)]
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
    /// Omit null and empty-list keys from JSON output (as `clove_show` does).
    #[arg(long)]
    pub compact: bool,
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
    /// Maximum depth (default 5; use `--depth 0` for no limit).
    #[arg(long)]
    pub depth: Option<usize>,
    /// Remove the depth limit (same as `--depth 0`).
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
    /// Filter by status (`open|in_progress|closed`). Repeatable: any of them.
    #[arg(long)]
    pub status: Vec<String>,
    /// Filter by type. Repeatable: any of them.
    #[arg(long = "type", value_name = "TYPE")]
    pub item_type: Vec<String>,
    /// Filter by label (canonicalized before matching). Repeatable: the item
    /// must carry **all** of them.
    #[arg(long)]
    pub label: Vec<String>,
    /// Filter by assignee.
    #[arg(long)]
    pub assignee: Option<String>,
    /// Filter by priority. Repeatable: any of them.
    #[arg(long)]
    pub priority: Vec<u8>,
    /// Keep only items whose id, title, or labels contain this text
    /// (case-insensitive). A filter, not a search — it never reads the body.
    #[arg(long, value_name = "TEXT")]
    pub q: Option<String>,
    /// Sort by `rank|priority|created|updated|id|status|type` (default `rank`:
    /// priority, then dependency order, then id).
    #[arg(long, value_name = "FIELD")]
    pub sort: Option<String>,
    /// Reverse the sort order.
    #[arg(long)]
    pub desc: bool,
    /// Maximum number of results (default 100; use `--limit 0` for no limit).
    #[arg(long)]
    pub limit: Option<usize>,
    /// Skip this many results.
    #[arg(long)]
    pub offset: Option<usize>,
    /// Comma-separated field projection.
    #[arg(long, value_name = "LIST")]
    pub fields: Option<String>,
    /// Omit null and empty-list keys from JSON output (as the MCP read tools do
    /// by default).
    #[arg(long)]
    pub compact: bool,
}

impl FilterArgs {
    /// The requested ordering, through the shared contract.
    pub fn order(&self) -> Result<clove_core::view::Order, clove_types::CloveError> {
        order_of(self.sort.as_deref(), self.desc)
    }

    /// The requested filter set, through the shared contract.
    ///
    /// One place rather than four: `ls`/`ready`/`blocked` all built their own
    /// `Filters::parse(...)` call, so a new filter flag had to be wired into
    /// each of them and `query` besides.
    pub fn filters(&self) -> Result<clove_core::view::Filters, clove_types::CloveError> {
        // `--priority` is a clap `u8` (so `--priority abc` is still a clap
        // error, exit 2, as it has always been); the shared parser takes words,
        // so the validated numbers are spelled back out.
        let priority: Vec<String> = self.priority.iter().map(u8::to_string).collect();
        clove_core::view::Filters::parse_multi(
            &self.status,
            &self.item_type,
            &self.label,
            self.assignee.as_deref(),
            &priority,
            self.q.as_deref(),
        )
    }
}

/// Decode `--sort`/`--desc` into the shared [`clove_core::view::Order`].
///
/// `--desc` is a boolean flag rather than `--dir <asc|desc>`; it maps onto the
/// same parser the web's `?dir=` uses so there is one validator, not two.
pub fn order_of(
    sort: Option<&str>,
    desc: bool,
) -> Result<clove_core::view::Order, clove_types::CloveError> {
    clove_core::view::Order::parse(sort, desc.then_some("desc"))
}

#[derive(Debug, Args)]
pub struct QueryArgs {
    /// A JSON filter object. If omitted and stdin is not a TTY, read it there.
    #[arg(long, value_name = "JSON")]
    pub filter: Option<String>,
    /// Comma-separated field projection.
    #[arg(long, value_name = "LIST")]
    pub fields: Option<String>,
    /// Omit null and empty-list keys from JSON output.
    #[arg(long)]
    pub compact: bool,
    /// Sort by `rank|priority|created|updated|id|status|type` (default `rank`).
    #[arg(long, value_name = "FIELD")]
    pub sort: Option<String>,
    /// Reverse the sort order.
    #[arg(long)]
    pub desc: bool,
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
    /// Skip this many of the *newest* comments, to page back through older
    /// ones. Named for its direction: unlike `--offset` on the list commands,
    /// this window is anchored at the newest end, not the start.
    #[arg(long)]
    pub skip_newest: Option<usize>,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// The search text.
    pub text: String,
    /// Sort by `rank|priority|created|updated|id|status|type`. Omitted, results
    /// are ranked by relevance (title hits, then labels, then body); naming a
    /// field replaces that ranking entirely.
    #[arg(long, value_name = "FIELD")]
    pub sort: Option<String>,
    /// Reverse the sort order (or, with no `--sort`, the relevance ranking).
    #[arg(long)]
    pub desc: bool,
    /// Maximum number of results (default 100; use `--limit 0` for no limit).
    #[arg(long)]
    pub limit: Option<usize>,
    /// Skip this many results.
    #[arg(long)]
    pub offset: Option<usize>,
    /// Comma-separated field projection.
    #[arg(long, value_name = "LIST")]
    pub fields: Option<String>,
    /// Omit null and empty-list keys from JSON output (as `clove_search` does).
    #[arg(long)]
    pub compact: bool,
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
    // The three window flags below are `requires = "history"`: a live report is
    // a single object with no series to filter or page, so `clove stats --limit
    // 5` had nothing to apply and used to succeed while ignoring the flag — the
    // advertised-and-ignored pattern the read-path roadmap §7 exists to remove.
    // The doc comments already said "With `--history`:"; clap now enforces it,
    // and prints that requirement instead of the flag quietly doing nothing.
    /// With `--history`: only snapshots at/after this RFC3339 timestamp.
    #[arg(long, value_name = "RFC3339", requires = "history")]
    pub since: Option<String>,
    /// With `--history`: show at most this many snapshots (default 100; use
    /// `--limit 0` for all).
    #[arg(long, value_name = "N", requires = "history")]
    pub limit: Option<usize>,
    /// With `--history`: skip this many snapshots.
    #[arg(long, value_name = "N", requires = "history")]
    pub offset: Option<usize>,
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
pub struct SetupArgs {
    /// Write to `~/.claude/` instead of `<project>/.claude/`.
    #[arg(long)]
    pub global: bool,
    /// Report what would change without writing anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Override the target `.claude` directory (testing).
    #[arg(long, value_name = "PATH", hide = true)]
    pub claude_dir: Option<Utf8PathBuf>,
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

/// `clove import <provider> [args…]` (PLUGIN_SYSTEM.md §4.2).
///
/// A router mirroring [`ExportArgs`]: the built-in native formats (`json`,
/// `jsonl`, clove's own restore) parse `rest` themselves (see `cmd::import`), and
/// any other provider (`tk`, `beads`, …) falls through to a
/// `clove-import-<provider>` plugin, with `rest` forwarded verbatim. Global flags
/// (e.g. `--format`) must precede the provider token, since everything after it
/// is captured raw for plugin forwarding.
#[derive(Debug, Args)]
pub struct ImportArgs {
    /// The source provider (built-in `json`/`jsonl`, or a
    /// `clove-import-<provider>` plugin, e.g. `tk` or `beads`).
    pub provider: String,
    /// Everything after the provider — the `<src>` and any provider flags
    /// (`--dry-run`) — forwarded to the plugin.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub rest: Vec<String>,
}

/// `clove export <provider> [args…]` (PLUGIN_SYSTEM.md §4.2).
///
/// A pure router mirroring [`ImportArgs`]: the built-in formats (`json`, `jsonl`)
/// parse `rest` themselves (see `cmd::export`), and any other provider falls
/// through to a `clove-export-<provider>` plugin.
#[derive(Debug, Args)]
pub struct ExportArgs {
    /// The export provider (built-in `json`/`jsonl`, or a
    /// `clove-export-<provider>` plugin).
    pub provider: String,
    /// Everything after the provider: the built-in flags (`[--out FILE]`) or the
    /// arguments forwarded to the plugin.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub rest: Vec<String>,
}

/// `clove sync <provider> [args…]` (PLUGIN_SYSTEM.md §4.2).
///
/// A pure router mirroring [`ImportArgs`]/[`ExportArgs`]: `sync` has **no**
/// built-in providers — every provider (including `github`) resolves to a
/// `clove-sync-<provider>` plugin, with `rest` forwarded verbatim. Global flags
/// (e.g. `--format`) must precede the provider token, since everything after it
/// is captured raw for plugin forwarding.
#[derive(Debug, Args)]
pub struct SyncArgs {
    /// The provider to sync with (a `clove-sync-<provider>` plugin, e.g.
    /// `github`).
    pub provider: String,
    /// Everything after the provider — the `owner/repo` and any provider flags
    /// (`--dry-run`, `--prefer`, `--no-comments`) — forwarded to the plugin.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub rest: Vec<String>,
}

/// The `clove export` output format. GitHub is handled by `clove sync github`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExportFormat {
    /// A single JSON envelope with a `data` array of all items.
    Json,
    /// One item per line (NDJSON) in clove's native item schema — the exact
    /// inverse of `clove import jsonl`. (A Beads-*native* export is the `beads`
    /// plugin, `clove export beads`, not this built-in.)
    Jsonl,
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

/// clap value-parser for `init --prefix`: reject anything `config.toml`'s loader
/// would later refuse, so a bad prefix fails at parse time (creating nothing)
/// instead of being written and bricking the repo on the next command.
fn parse_id_prefix(raw: &str) -> Result<String, String> {
    if clove_core::config::is_valid_prefix(raw) {
        Ok(raw.to_owned())
    } else {
        Err(format!(
            "id_prefix `{raw}` must match ^[a-z][a-z0-9]{{0,7}}$"
        ))
    }
}
