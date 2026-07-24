//! [`PluginContext`] — the typed materialization of the host↔plugin environment
//! contract (`PLUGIN_SYSTEM.md` §6.2/§6.5).
//!
//! The host resolves repo discovery, config load, and format precedence once and
//! exports the answers as `CLOVE_*` vars; this reads them back into the same
//! shape as the host's own [`context::Ctx`](../../clove/src/context.rs). A plugin
//! author therefore touches [`std::env`] never and can never disagree with the
//! host about where the repo is or which envelope the user asked for.

use camino::Utf8PathBuf;
use clove_core::{CloveConfig, ItemStore, OutputFormat};
use clove_types::CloveError;

/// Terminal color preference (`$CLOVE_COLOR`).
///
/// The plugin-side mirror of the host's `cli::ColorChoice`; parsed from the
/// lowercase wire spelling (`auto` | `always` | `never`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginColor {
    Auto,
    Always,
    Never,
}

impl PluginColor {
    /// Parse the wire spelling (`auto` | `always` | `never`).
    pub fn parse(value: &str) -> Option<PluginColor> {
        match value {
            "auto" => Some(PluginColor::Auto),
            "always" => Some(PluginColor::Always),
            "never" => Some(PluginColor::Never),
            _ => None,
        }
    }
}

impl std::str::FromStr for PluginColor {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        PluginColor::parse(s).ok_or(())
    }
}

/// The typed §6.2 environment — every value the host already resolved.
///
/// This is the plugin-side mirror of the host's `context::Ctx`: same fields,
/// sourced from env instead of from `discover()`. Every path is absolute and
/// UTF-8; every scalar is the *resolved* one (flag > env > config already
/// collapsed by the host).
#[derive(Debug, Clone)]
pub struct PluginContext {
    // identity & contract
    /// Absolute path to the host `clove` binary, for callback (`$CLOVE`).
    pub clove_bin: Utf8PathBuf,
    /// Host semver, e.g. `0.1.0` (`$CLOVE_VERSION`).
    pub host_version: String,
    /// Item on-disk schema version the host writes (`$CLOVE_SCHEMA`).
    pub schema: u32,
    /// Contract version, starts at `1` (`$CLOVE_PLUGIN_API`).
    pub api: u32,
    /// The dispatch path that reached the plugin, e.g. `sync` (`$CLOVE_COMMAND`).
    pub command: String,
    /// The provider token for a multiplexer plugin; `None` for generic plugins
    /// (`$CLOVE_PROVIDER`, omitted when absent).
    pub provider: Option<String>,
    // repository location
    /// The resolved `.clove/` directory (`$CLOVE_DIR`).
    pub clove_dir: Utf8PathBuf,
    /// Repo root, parent of `.clove/` (`$CLOVE_ROOT`).
    pub root: Utf8PathBuf,
    /// `.clove/issues/` (`$CLOVE_ISSUES_DIR`).
    pub issues_dir: Utf8PathBuf,
    /// `.clove/index.db`; may not exist (`$CLOVE_DB_PATH`).
    pub db_path: Utf8PathBuf,
    /// `.clove/sync/` per-repo sync fingerprints (`$CLOVE_SYNC_DIR`).
    pub sync_dir: Utf8PathBuf,
    /// Path to `.clove/config.toml` (`$CLOVE_CONFIG_PATH`).
    pub config_path: Utf8PathBuf,
    // resolved config & output
    /// The repo's id prefix, needed to mint new ids (`$CLOVE_ID_PREFIX`).
    pub id_prefix: String,
    /// The envelope the user asked for (`$CLOVE_FORMAT`).
    pub format: OutputFormat,
    /// Terminal color preference (`$CLOVE_COLOR`).
    pub color: PluginColor,
    /// Suppress informational stderr (`$CLOVE_QUIET`).
    pub quiet: bool,
    /// The `--no-index` global flag (`$CLOVE_NO_INDEX`).
    pub no_index: bool,
    /// The `--deep` staleness flag (`$CLOVE_DEEP`).
    pub deep: bool,
}

/// A failure to materialize the §6.2 environment into a [`PluginContext`].
///
/// Each variant names the offending var so a plugin launched outside `clove`
/// (where the contract vars are absent or malformed) fails loudly and legibly.
/// It converts to a validation-class [`CloveError`] so [`crate::run`] can render
/// it through the standard error envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginEnvError {
    /// A required `CLOVE_*` var was absent.
    MissingVar { name: &'static str },
    /// A boolean var was not exactly `0` or `1`.
    BadBool { name: &'static str, value: String },
    /// An enum var was not a recognized wire spelling.
    BadEnum { name: &'static str, value: String },
    /// An integer var did not parse.
    BadInt { name: &'static str, value: String },
}

impl std::fmt::Display for PluginEnvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PluginEnvError::MissingVar { name } => {
                write!(f, "missing required environment variable {name}")
            }
            PluginEnvError::BadBool { name, value } => {
                write!(f, "{name} must be `0` or `1`, got {value:?}")
            }
            PluginEnvError::BadEnum { name, value } => {
                write!(f, "{name} has an unrecognized value {value:?}")
            }
            PluginEnvError::BadInt { name, value } => {
                write!(f, "{name} must be an integer, got {value:?}")
            }
        }
    }
}

impl std::error::Error for PluginEnvError {}

impl From<PluginEnvError> for CloveError {
    fn from(err: PluginEnvError) -> Self {
        let field = match err {
            PluginEnvError::MissingVar { name }
            | PluginEnvError::BadBool { name, .. }
            | PluginEnvError::BadEnum { name, .. }
            | PluginEnvError::BadInt { name, .. } => name,
        };
        CloveError::InvalidField {
            field: field.to_owned(),
            reason: err.to_string(),
        }
    }
}

impl PluginContext {
    /// Read every §6.2 `CLOVE_*` var into a typed context.
    ///
    /// A missing required var is [`PluginEnvError::MissingVar`]; a boolean that is
    /// not exactly `0`/`1` is [`PluginEnvError::BadBool`]; a non-integer
    /// `CLOVE_SCHEMA`/`CLOVE_PLUGIN_API` is [`PluginEnvError::BadInt`]; an
    /// unrecognized `CLOVE_FORMAT`/`CLOVE_COLOR` is [`PluginEnvError::BadEnum`].
    /// `CLOVE_PROVIDER` is optional (absent → `None`); the contract omits it
    /// rather than setting it empty, so there is no empty-string special case.
    pub fn from_env() -> Result<PluginContext, PluginEnvError> {
        Ok(PluginContext {
            clove_bin: req_path("CLOVE")?,
            host_version: req("CLOVE_VERSION")?,
            schema: req_int("CLOVE_SCHEMA")?,
            api: req_int("CLOVE_PLUGIN_API")?,
            command: req("CLOVE_COMMAND")?,
            provider: opt("CLOVE_PROVIDER"),
            clove_dir: req_path("CLOVE_DIR")?,
            root: req_path("CLOVE_ROOT")?,
            issues_dir: req_path("CLOVE_ISSUES_DIR")?,
            db_path: req_path("CLOVE_DB_PATH")?,
            sync_dir: req_path("CLOVE_SYNC_DIR")?,
            config_path: req_path("CLOVE_CONFIG_PATH")?,
            id_prefix: req("CLOVE_ID_PREFIX")?,
            format: req_format("CLOVE_FORMAT")?,
            color: req_color("CLOVE_COLOR")?,
            quiet: req_bool("CLOVE_QUIET")?,
            no_index: req_bool("CLOVE_NO_INDEX")?,
            deep: req_bool("CLOVE_DEEP")?,
        })
    }

    /// The capability this dispatch reached the plugin for, as a `provides` token:
    /// `"<command>:<provider>"` for a multiplexer plugin, or bare `"<command>"` for
    /// a generic one. A multi-capability plugin compares this against its own
    /// [`PluginInfo::provides`](crate::PluginInfo) set to decide whether it was
    /// handed a capability it implements (`PLUGIN_SYSTEM.md` §4.2).
    pub fn capability(&self) -> String {
        match &self.provider {
            Some(provider) => format!("{}:{}", self.command, provider),
            None => self.command.clone(),
        }
    }

    /// Open the file store the fat-plugin way (§6.4A): `ItemStore::new(root)`.
    ///
    /// Keeps the plugin on the unified write path (mutating through
    /// `clove_core::apply_edit`) with one call.
    pub fn open_store(&self) -> ItemStore {
        ItemStore::new(self.root.clone())
    }

    /// Re-read and validate `config.toml`. The host already validated it; this
    /// re-loads it from [`self.root`](Self::root).
    pub fn load_config(&self) -> Result<CloveConfig, CloveError> {
        clove_core::load_config(&self.root)
    }
}

/// A required string var.
fn req(name: &'static str) -> Result<String, PluginEnvError> {
    std::env::var(name).map_err(|_| PluginEnvError::MissingVar { name })
}

/// A required path var.
fn req_path(name: &'static str) -> Result<Utf8PathBuf, PluginEnvError> {
    req(name).map(Utf8PathBuf::from)
}

/// An optional var: absent (or unreadable) → `None`. Per §6.2 a logically absent
/// var is omitted, never set empty, so no empty-string special case is needed.
fn opt(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// A required integer var.
fn req_int(name: &'static str) -> Result<u32, PluginEnvError> {
    let raw = req(name)?;
    raw.parse()
        .map_err(|_| PluginEnvError::BadInt { name, value: raw })
}

/// A required boolean var: exactly `0` or `1`.
fn req_bool(name: &'static str) -> Result<bool, PluginEnvError> {
    let raw = req(name)?;
    match raw.as_str() {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(PluginEnvError::BadBool { name, value: raw }),
    }
}

/// A required output-format var, via [`OutputFormat::parse`].
fn req_format(name: &'static str) -> Result<OutputFormat, PluginEnvError> {
    let raw = req(name)?;
    OutputFormat::parse(&raw).ok_or(PluginEnvError::BadEnum { name, value: raw })
}

/// A required color var, via [`PluginColor::parse`].
fn req_color(name: &'static str) -> Result<PluginColor, PluginEnvError> {
    let raw = req(name)?;
    PluginColor::parse(&raw).ok_or(PluginEnvError::BadEnum { name, value: raw })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// The process environment is global; serialize the env-mutating tests so a
    /// full var-set installed by one does not leak into another.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Every §6.2 var, including an optional `CLOVE_PROVIDER`.
    const FULL_ENV: &[(&str, &str)] = &[
        ("CLOVE", "/usr/local/bin/clove"),
        ("CLOVE_VERSION", "0.1.0"),
        ("CLOVE_SCHEMA", "1"),
        ("CLOVE_PLUGIN_API", "1"),
        ("CLOVE_COMMAND", "sync"),
        ("CLOVE_PROVIDER", "github"),
        ("CLOVE_DIR", "/repo/.clove"),
        ("CLOVE_ROOT", "/repo"),
        ("CLOVE_ISSUES_DIR", "/repo/.clove/issues"),
        ("CLOVE_DB_PATH", "/repo/.clove/index.db"),
        ("CLOVE_SYNC_DIR", "/repo/.clove/sync"),
        ("CLOVE_CONFIG_PATH", "/repo/.clove/config.toml"),
        ("CLOVE_ID_PREFIX", "clove"),
        ("CLOVE_FORMAT", "json"),
        ("CLOVE_COLOR", "never"),
        ("CLOVE_QUIET", "1"),
        ("CLOVE_NO_INDEX", "0"),
        ("CLOVE_DEEP", "1"),
    ];

    const ALL_VARS: &[&str] = &[
        "CLOVE",
        "CLOVE_VERSION",
        "CLOVE_SCHEMA",
        "CLOVE_PLUGIN_API",
        "CLOVE_COMMAND",
        "CLOVE_PROVIDER",
        "CLOVE_DIR",
        "CLOVE_ROOT",
        "CLOVE_ISSUES_DIR",
        "CLOVE_DB_PATH",
        "CLOVE_SYNC_DIR",
        "CLOVE_CONFIG_PATH",
        "CLOVE_ID_PREFIX",
        "CLOVE_FORMAT",
        "CLOVE_COLOR",
        "CLOVE_QUIET",
        "CLOVE_NO_INDEX",
        "CLOVE_DEEP",
    ];

    fn clear_all() {
        for name in ALL_VARS {
            std::env::remove_var(name);
        }
    }

    fn install(pairs: &[(&str, &str)]) {
        clear_all();
        for (name, value) in pairs {
            std::env::set_var(name, value);
        }
    }

    #[test]
    fn round_trips_a_full_env() {
        let _guard = env_lock();
        install(FULL_ENV);
        let cx = PluginContext::from_env().expect("full env should materialize");
        assert_eq!(cx.clove_bin, Utf8PathBuf::from("/usr/local/bin/clove"));
        assert_eq!(cx.host_version, "0.1.0");
        assert_eq!(cx.schema, 1);
        assert_eq!(cx.api, 1);
        assert_eq!(cx.command, "sync");
        assert_eq!(cx.provider.as_deref(), Some("github"));
        assert_eq!(cx.clove_dir, Utf8PathBuf::from("/repo/.clove"));
        assert_eq!(cx.root, Utf8PathBuf::from("/repo"));
        assert_eq!(cx.issues_dir, Utf8PathBuf::from("/repo/.clove/issues"));
        assert_eq!(cx.db_path, Utf8PathBuf::from("/repo/.clove/index.db"));
        assert_eq!(cx.sync_dir, Utf8PathBuf::from("/repo/.clove/sync"));
        assert_eq!(
            cx.config_path,
            Utf8PathBuf::from("/repo/.clove/config.toml")
        );
        assert_eq!(cx.id_prefix, "clove");
        assert_eq!(cx.format, OutputFormat::Json);
        assert_eq!(cx.color, PluginColor::Never);
        assert!(cx.quiet);
        assert!(!cx.no_index);
        assert!(cx.deep);
        clear_all();
    }

    #[test]
    fn capability_token_reflects_command_and_provider() {
        let _guard = env_lock();
        install(FULL_ENV);
        let cx = PluginContext::from_env().expect("full env should materialize");
        assert_eq!(cx.capability(), "sync:github");
        std::env::remove_var("CLOVE_PROVIDER");
        let cx = PluginContext::from_env().expect("provider is optional");
        assert_eq!(cx.capability(), "sync");
        clear_all();
    }

    #[test]
    fn unsupported_capability_maps_to_wire_code_and_exit_2() {
        // Locks the contract every multiplexer main's guard arm relies on: a
        // structurally-reached-but-unimplemented capability fails as exit 2 with the
        // distinct `UNSUPPORTED_CAPABILITY` code (§7.2), not a panic or exit 4.
        let _guard = env_lock();
        install(FULL_ENV);
        let cx = PluginContext::from_env().expect("full env should materialize");
        let info = crate::PluginInfo {
            name: "clove-import-tk",
            version: "0.1.0",
            about: "tk importer",
            provides: &["import:tk"],
        };
        let err = crate::unsupported_capability(&info, &cx);
        assert_eq!(clove_types::error_code(&err), ("UNSUPPORTED_CAPABILITY", 2));
        clear_all();
    }

    #[test]
    fn optional_provider_absent_is_none() {
        let _guard = env_lock();
        install(FULL_ENV);
        std::env::remove_var("CLOVE_PROVIDER");
        let cx = PluginContext::from_env().expect("provider is optional");
        assert_eq!(cx.provider, None);
        clear_all();
    }

    #[test]
    fn missing_required_var_is_reported() {
        let _guard = env_lock();
        install(FULL_ENV);
        std::env::remove_var("CLOVE_ROOT");
        assert_eq!(
            PluginContext::from_env().unwrap_err(),
            PluginEnvError::MissingVar { name: "CLOVE_ROOT" }
        );
        clear_all();
    }

    #[test]
    fn strict_bool_rejects_non_0_1() {
        let _guard = env_lock();
        install(FULL_ENV);
        std::env::set_var("CLOVE_QUIET", "true");
        assert_eq!(
            PluginContext::from_env().unwrap_err(),
            PluginEnvError::BadBool {
                name: "CLOVE_QUIET",
                value: "true".to_owned(),
            }
        );
        clear_all();
    }

    #[test]
    fn bad_enum_rejects_unknown_format() {
        let _guard = env_lock();
        install(FULL_ENV);
        std::env::set_var("CLOVE_FORMAT", "yaml");
        assert_eq!(
            PluginContext::from_env().unwrap_err(),
            PluginEnvError::BadEnum {
                name: "CLOVE_FORMAT",
                value: "yaml".to_owned(),
            }
        );
        clear_all();
    }

    #[test]
    fn bad_int_rejects_non_integer_schema() {
        let _guard = env_lock();
        install(FULL_ENV);
        std::env::set_var("CLOVE_SCHEMA", "v1");
        assert_eq!(
            PluginContext::from_env().unwrap_err(),
            PluginEnvError::BadInt {
                name: "CLOVE_SCHEMA",
                value: "v1".to_owned(),
            }
        );
        clear_all();
    }

    #[test]
    fn env_error_maps_to_validation_class() {
        let err: CloveError = PluginEnvError::MissingVar { name: "CLOVE_ROOT" }.into();
        assert!(matches!(err, CloveError::InvalidField { .. }));
        // Validation class is exit 4 (see crates/clove/src/exit.rs).
        assert_eq!(clove_types::error_code(&err).1, 4);
    }

    #[test]
    fn color_parses_wire_spellings() {
        assert_eq!("auto".parse(), Ok(PluginColor::Auto));
        assert_eq!("always".parse(), Ok(PluginColor::Always));
        assert_eq!("never".parse(), Ok(PluginColor::Never));
        assert_eq!("bright".parse::<PluginColor>(), Err(()));
    }
}
