//! Repository configuration: `.clove/config.toml` (DESIGN.md §10).
//!
//! Loading precedence (low → high): compiled-in defaults → `.clove/config.toml`
//! → `CLOVE_*` environment variables. The `--format` CLI flag (highest) is
//! applied by the CLI layer, above everything here.

use camino::Utf8Path;
use serde::{Deserialize, Serialize};

use crate::error::CloveError;
use crate::model::ItemType;

/// Current `config.toml` schema version (distinct from item schema).
pub const CURRENT_CONFIG_SCHEMA: u32 = 1;

/// The exact `.clove/.gitignore` entries (DESIGN §2.1 / §8.2). These keep the
/// rebuildable SQLite cache and the daemon's socket/pid/lock files out of git —
/// committing them would put derived state into the source of truth. `clove init`
/// writes this set (LF endings on every platform) and `clove doctor` verifies it
/// is present (`GITIGNORE_DRIFT`), so the canonical list lives here, shared by
/// both rather than duplicated.
pub const GITIGNORE_ENTRIES: [&str; 9] = [
    "index.db",
    "*.db-shm",
    "*.db-wal",
    "daemon.sock",
    "daemon.pid",
    "reindex.lock",
    "daemon.lock",
    "index.db.tmp",
    // Per-clone GitHub sync bookkeeping (`sync/github/<owner>__<repo>.json`):
    // local last-sync clocks, rebuildable, must not enter the source of truth.
    "sync/",
];

/// Minimum / maximum number of random characters in a generated id.
pub const MIN_ID_LENGTH: u8 = 4;
pub const MAX_ID_LENGTH: u8 = 12;

/// Output format for CLI responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    #[default]
    Human,
    Json,
    Jsonl,
}

impl OutputFormat {
    /// Parse a format name (used for `CLOVE_FORMAT` and the `--format` flag).
    pub fn parse(value: &str) -> Option<OutputFormat> {
        match value.trim().to_ascii_lowercase().as_str() {
            "human" => Some(OutputFormat::Human),
            "json" => Some(OutputFormat::Json),
            "jsonl" => Some(OutputFormat::Jsonl),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            OutputFormat::Human => "human",
            OutputFormat::Json => "json",
            OutputFormat::Jsonl => "jsonl",
        }
    }
}

/// `[index]` configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexConfig {
    #[serde(default = "default_true")]
    pub auto_refresh: bool,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self { auto_refresh: true }
    }
}

/// `[daemon]` configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    #[serde(default)]
    pub git_sync: bool,
    #[serde(default = "default_debounce_ms")]
    pub watch_debounce_ms: u64,
    #[serde(default = "default_idle_shutdown_min")]
    pub idle_shutdown_min: u64,
    #[serde(default = "default_stats_snapshot_min")]
    pub stats_snapshot_min: u64,
    /// Minutes between automatic two-way GitHub syncs (`0` = disabled, the
    /// default). Requires `github_sync_repo` to be set.
    #[serde(default)]
    pub github_sync_interval_min: u64,
    /// The `owner/repo` the daemon periodically syncs with (when
    /// `github_sync_interval_min > 0`).
    #[serde(default)]
    pub github_sync_repo: Option<String>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            git_sync: false,
            watch_debounce_ms: default_debounce_ms(),
            idle_shutdown_min: default_idle_shutdown_min(),
            stats_snapshot_min: default_stats_snapshot_min(),
            github_sync_interval_min: 0,
            github_sync_repo: None,
        }
    }
}

/// `[web]` configuration — the web UI served by `clove serve` and (by default)
/// the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    /// Whether a running daemon serves the web UI automatically.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// The TCP port the web UI binds (loopback only).
    #[serde(default = "default_web_port")]
    pub port: u16,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            port: default_web_port(),
        }
    }
}

/// The full repository configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloveConfig {
    #[serde(default = "default_config_schema")]
    pub config_schema: u32,
    #[serde(default = "default_prefix")]
    pub id_prefix: String,
    #[serde(default = "default_id_length")]
    pub id_length: u8,
    #[serde(default)]
    pub default_type: ItemType,
    #[serde(default)]
    pub default_format: OutputFormat,
    #[serde(default)]
    pub index: IndexConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub web: WebConfig,
}

impl Default for CloveConfig {
    fn default() -> Self {
        Self {
            config_schema: CURRENT_CONFIG_SCHEMA,
            id_prefix: default_prefix(),
            id_length: default_id_length(),
            default_type: ItemType::default(),
            default_format: OutputFormat::default(),
            index: IndexConfig::default(),
            daemon: DaemonConfig::default(),
            web: WebConfig::default(),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_web_port() -> u16 {
    7373
}
fn default_debounce_ms() -> u64 {
    200
}
/// Default idle self-shutdown window (minutes). Every clove command is a
/// heartbeat that resets this timer, so an actively-used daemon never times out;
/// it only self-terminates after this long with *no* clove activity at all. There
/// is no auto-restart yet (a future MCP session would hold a heartbeat), so the
/// default is generous — 4 hours survives normal workday gaps (meetings, lunch)
/// and cleans up overnight. `0` disables idle shutdown entirely.
fn default_idle_shutdown_min() -> u64 {
    240
}
/// Default interval (minutes) at which a running daemon records a work-item
/// analytics snapshot into the index's durable `snapshots` history (the data
/// behind `clove stats --history`). Hourly captures a useful trend without
/// meaningful cost or growth (rows are tiny and the index is local/gitignored).
/// `0` disables auto-snapshots; manual `clove stats --snapshot` still works.
fn default_stats_snapshot_min() -> u64 {
    60
}
fn default_config_schema() -> u32 {
    CURRENT_CONFIG_SCHEMA
}
fn default_id_length() -> u8 {
    8
}
fn default_prefix() -> String {
    "proj".to_owned()
}

impl CloveConfig {
    /// The path to `config.toml` under a repo root.
    pub fn path_in(repo_root: &Utf8Path) -> camino::Utf8PathBuf {
        repo_root.join(".clove").join("config.toml")
    }

    /// Parse a config from TOML text (errors carry `path` for context).
    pub fn from_toml_str(text: &str, path: &Utf8Path) -> Result<CloveConfig, CloveError> {
        toml::from_str(text).map_err(|err| CloveError::Config {
            path: path.to_owned(),
            message: err.to_string(),
        })
    }

    /// Validate field invariants (DESIGN.md §10).
    pub fn validate(&self, path: &Utf8Path) -> Result<(), CloveError> {
        let invalid = |message: String| CloveError::Config {
            path: path.to_owned(),
            message,
        };
        if self.config_schema != CURRENT_CONFIG_SCHEMA {
            return Err(invalid(format!(
                "unsupported config_schema {} (supported: {CURRENT_CONFIG_SCHEMA})",
                self.config_schema
            )));
        }
        if !is_valid_prefix(&self.id_prefix) {
            return Err(invalid(format!(
                "id_prefix `{}` must match ^[a-z][a-z0-9]{{0,7}}$",
                self.id_prefix
            )));
        }
        if self.id_length < MIN_ID_LENGTH || self.id_length > MAX_ID_LENGTH {
            return Err(invalid(format!(
                "id_length {} must be between {MIN_ID_LENGTH} and {MAX_ID_LENGTH}",
                self.id_length
            )));
        }
        Ok(())
    }

    /// Apply `CLOVE_*` environment overrides using `get` to read variables.
    fn apply_env<F>(&mut self, get: F)
    where
        F: Fn(&str) -> Option<String>,
    {
        if let Some(prefix) = get("CLOVE_ID_PREFIX") {
            self.id_prefix = prefix;
        }
        if let Some(format) = get("CLOVE_FORMAT").and_then(|v| OutputFormat::parse(&v)) {
            self.default_format = format;
        }
    }
}

/// Load the configuration for the repository rooted at `repo_root`.
///
/// Reads `.clove/config.toml` if present (otherwise uses defaults with an
/// id-prefix derived from the repo directory name), applies `CLOVE_*` env
/// overrides, then validates.
pub fn load_config(repo_root: &Utf8Path) -> Result<CloveConfig, CloveError> {
    let path = CloveConfig::path_in(repo_root);

    let mut config = if path.exists() {
        guard_not_symlink_escape(&path, repo_root)?;
        let text = std::fs::read_to_string(&path).map_err(|source| CloveError::Io {
            path: path.clone(),
            source,
        })?;
        CloveConfig::from_toml_str(&text, &path)?
    } else {
        CloveConfig {
            id_prefix: derive_prefix(repo_root),
            ..Default::default()
        }
    };

    config.apply_env(|key| std::env::var(key).ok());
    config.validate(&path)?;
    Ok(config)
}

/// Derive a default id prefix from a repository directory name: lowercase, keep
/// alphanumerics, ensure it starts with a letter, cap at 4 characters. Falls
/// back to `item` when nothing usable remains.
pub fn derive_prefix(repo_root: &Utf8Path) -> String {
    let name = repo_root.file_name().unwrap_or("item");
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .skip_while(|c| c.is_ascii_digit()) // must start with a letter
        .take(4)
        .collect();
    if cleaned.is_empty() {
        "item".to_owned()
    } else {
        cleaned
    }
}

/// Reject a `config.toml` that is a symlink resolving outside the repo root.
fn guard_not_symlink_escape(path: &Utf8Path, repo_root: &Utf8Path) -> Result<(), CloveError> {
    let meta = std::fs::symlink_metadata(path).map_err(|source| CloveError::Io {
        path: path.to_owned(),
        source,
    })?;
    if !meta.file_type().is_symlink() {
        return Ok(());
    }
    let resolved = path.canonicalize_utf8().map_err(|source| CloveError::Io {
        path: path.to_owned(),
        source,
    })?;
    let canonical_root = repo_root
        .canonicalize_utf8()
        .unwrap_or_else(|_| repo_root.to_owned());
    if resolved.starts_with(&canonical_root) {
        Ok(())
    } else {
        Err(CloveError::Config {
            path: path.to_owned(),
            message: "config.toml is a symlink pointing outside the repository".to_owned(),
        })
    }
}

/// Whether `s` is a valid id prefix: `^[a-z][a-z0-9]{0,7}$`.
fn is_valid_prefix(s: &str) -> bool {
    if s.is_empty() || s.len() > 8 {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().expect("non-empty");
    first.is_ascii_lowercase() && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> camino::Utf8PathBuf {
        camino::Utf8PathBuf::from("/repo/.clove/config.toml")
    }

    #[test]
    fn defaults_are_valid() {
        let config = CloveConfig::default();
        assert!(config.validate(&p()).is_ok());
        assert_eq!(config.id_length, 8);
        assert_eq!(config.default_format, OutputFormat::Human);
        assert!(config.index.auto_refresh);
        assert!(!config.daemon.git_sync);
    }

    #[test]
    fn parses_full_toml() {
        let text = r#"
config_schema = 1
id_prefix = "clove"
id_length = 6
default_type = "bug"
default_format = "json"
[index]
auto_refresh = false
[daemon]
git_sync = true
watch_debounce_ms = 300
idle_shutdown_min = 5
"#;
        let config = CloveConfig::from_toml_str(text, &p()).unwrap();
        config.validate(&p()).unwrap();
        assert_eq!(config.id_prefix, "clove");
        assert_eq!(config.id_length, 6);
        assert_eq!(config.default_type, ItemType::Bug);
        assert_eq!(config.default_format, OutputFormat::Json);
        assert!(!config.index.auto_refresh);
        assert!(config.daemon.git_sync);
        assert_eq!(config.daemon.watch_debounce_ms, 300);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let config = CloveConfig::from_toml_str("id_prefix = \"x\"\n", &p()).unwrap();
        assert_eq!(config.id_prefix, "x");
        assert_eq!(config.id_length, 8);
        assert_eq!(config.default_format, OutputFormat::Human);
    }

    #[test]
    fn rejects_invalid_prefix() {
        for bad in ["Proj", "9roj", "has-dash", "toolongprefix", ""] {
            let config = CloveConfig {
                id_prefix: bad.to_owned(),
                ..Default::default()
            };
            assert!(
                config.validate(&p()).is_err(),
                "should reject prefix {bad:?}"
            );
        }
    }

    #[test]
    fn rejects_out_of_range_id_length() {
        for bad in [0u8, 3, 13, 255] {
            let config = CloveConfig {
                id_length: bad,
                ..Default::default()
            };
            assert!(
                config.validate(&p()).is_err(),
                "should reject id_length {bad}"
            );
        }
        for good in [4u8, 8, 12] {
            let config = CloveConfig {
                id_length: good,
                ..Default::default()
            };
            assert!(config.validate(&p()).is_ok());
        }
    }

    #[test]
    fn rejects_unknown_config_schema() {
        let config = CloveConfig {
            config_schema: 99,
            ..Default::default()
        };
        assert!(config.validate(&p()).is_err());
    }

    #[test]
    fn rejects_unknown_toml_field() {
        assert!(CloveConfig::from_toml_str("bogus_field = 1\n", &p()).is_err());
    }

    #[test]
    fn env_overrides_take_precedence_over_file() {
        // file/default value
        let mut config = CloveConfig::from_toml_str("id_prefix = \"file\"\n", &p()).unwrap();
        assert_eq!(config.default_format, OutputFormat::Human);
        // env overrides
        let env: std::collections::HashMap<&str, &str> =
            [("CLOVE_FORMAT", "json"), ("CLOVE_ID_PREFIX", "envp")]
                .into_iter()
                .collect();
        config.apply_env(|k| env.get(k).map(|v| v.to_string()));
        assert_eq!(config.default_format, OutputFormat::Json);
        assert_eq!(config.id_prefix, "envp");
    }

    #[test]
    fn derive_prefix_from_repo_name() {
        assert_eq!(derive_prefix(Utf8Path::new("/x/clove")), "clov");
        assert_eq!(derive_prefix(Utf8Path::new("/x/MyProject")), "mypr");
        assert_eq!(derive_prefix(Utf8Path::new("/x/123abc")), "abc");
        assert_eq!(derive_prefix(Utf8Path::new("/x/999")), "item");
    }

    #[test]
    fn load_config_uses_defaults_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        std::fs::create_dir_all(root.join(".clove")).unwrap();
        // No config.toml present.
        let config = load_config(root).unwrap();
        // Prefix derived from the temp dir name (always valid since validated).
        assert!(
            is_valid_prefix(&config.id_prefix),
            "derived `{}`",
            config.id_prefix
        );
        assert_eq!(config.id_length, 8);
    }

    #[test]
    fn load_config_reads_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        std::fs::create_dir_all(root.join(".clove")).unwrap();
        std::fs::write(
            CloveConfig::path_in(root),
            "id_prefix = \"abcd\"\nid_length = 5\n",
        )
        .unwrap();
        let config = load_config(root).unwrap();
        assert_eq!(config.id_prefix, "abcd");
        assert_eq!(config.id_length, 5);
    }
}
