//! `clove comment` / `clove comments` (T-CLI12).

use clove_core::{add_comment, list_comments, OutputFormat};
use clove_types::CloveError;
use serde_json::{json, Value};

use crate::context::{rel_to_root, Ctx};
use crate::output::print_json_success;
use crate::util::parse_id;

/// Resolve the comment author, in priority order: `CLOVE_AUTHOR`,
/// `GIT_AUTHOR_EMAIL`, `git config user.email`, `git config user.name`, `$USER`,
/// else `unknown`. Every source is best-effort and blank values are skipped, so
/// a repo with a configured git identity attributes comments to that identity
/// even when no clove-specific env var is set.
fn author() -> String {
    resolve_author(
        std::env::var("CLOVE_AUTHOR").ok(),
        std::env::var("GIT_AUTHOR_EMAIL").ok(),
        git_config("user.email"),
        git_config("user.name"),
        std::env::var("USER").ok(),
    )
}

/// Pure resolution of the author precedence chain: the first non-blank source
/// wins, trimmed; `unknown` if all are empty. Kept separate from the env/git
/// lookups so the ordering is unit-testable without touching process state.
fn resolve_author(
    clove_author: Option<String>,
    git_author_email: Option<String>,
    git_user_email: Option<String>,
    git_user_name: Option<String>,
    user: Option<String>,
) -> String {
    [
        clove_author,
        git_author_email,
        git_user_email,
        git_user_name,
        user,
    ]
    .into_iter()
    .flatten()
    .map(|value| value.trim().to_owned())
    .find(|value| !value.is_empty())
    .unwrap_or_else(|| "unknown".to_owned())
}

/// Read a single git config value (`git config --get <key>`), best-effort:
/// returns `None` if git is absent, the key is unset, or the value is blank.
fn git_config(key: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["config", "--get", key])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

pub fn add(
    ctx: &Ctx,
    format: OutputFormat,
    id: &str,
    message: &str,
    quiet: bool,
) -> Result<(), CloveError> {
    let id = parse_id(id)?;
    if !ctx.store.exists(&id) {
        return Err(CloveError::NotFound { id: id.to_string() });
    }
    let path = add_comment(&ctx.issues_dir, &id, &author(), message)?;
    let rel = rel_to_root(&ctx.root, &path);
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_success(
            json!({ "id": id.as_str(), "path": rel.as_str() }),
            json!({ "warnings": [] }),
        ),
        OutputFormat::Human => {
            if !quiet {
                println!("added comment to {}", id.as_str());
            }
        }
    }
    Ok(())
}

pub fn list(
    ctx: &Ctx,
    format: OutputFormat,
    id: &str,
    limit: Option<usize>,
) -> Result<(), CloveError> {
    let id = parse_id(id)?;
    if !ctx.store.exists(&id) {
        return Err(CloveError::NotFound { id: id.to_string() });
    }
    let mut comments = list_comments(&ctx.issues_dir, &id)?;
    if let Some(n) = limit {
        if comments.len() > n {
            comments = comments.split_off(comments.len() - n);
        }
    }

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            let values: Vec<Value> = comments
                .iter()
                .map(|c| {
                    json!({
                        "author": c.author,
                        "timestamp": c.timestamp.to_rfc3339(),
                        "body": c.body,
                    })
                })
                .collect();
            print_json_success(Value::Array(values), json!({ "warnings": [] }));
        }
        OutputFormat::Human => {
            for c in &comments {
                println!("{}  {}", c.timestamp.to_rfc3339(), c.author);
                println!("{}\n", c.body.trim_end());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::resolve_author;

    fn s(v: &str) -> Option<String> {
        Some(v.to_owned())
    }

    #[test]
    fn clove_author_wins_over_everything() {
        let got = resolve_author(
            s("clove-me"),
            s("git@ex"),
            s("cfg@ex"),
            s("Cfg Name"),
            s("login"),
        );
        assert_eq!(got, "clove-me");
    }

    #[test]
    fn falls_back_through_the_chain_in_order() {
        // GIT_AUTHOR_EMAIL when CLOVE_AUTHOR is absent.
        assert_eq!(
            resolve_author(None, s("git@ex"), s("cfg@ex"), s("Cfg Name"), s("login")),
            "git@ex"
        );
        // git config user.email next.
        assert_eq!(
            resolve_author(None, None, s("cfg@ex"), s("Cfg Name"), s("login")),
            "cfg@ex"
        );
        // then git config user.name — the case this fix adds.
        assert_eq!(
            resolve_author(None, None, None, s("Cfg Name"), s("login")),
            "Cfg Name"
        );
        // then $USER.
        assert_eq!(resolve_author(None, None, None, None, s("login")), "login");
    }

    #[test]
    fn blank_sources_are_skipped_not_selected() {
        // An empty CLOVE_AUTHOR must not shadow a real git identity.
        assert_eq!(
            resolve_author(s("   "), None, s("cfg@ex"), None, None),
            "cfg@ex"
        );
    }

    #[test]
    fn unknown_only_when_all_sources_empty() {
        assert_eq!(resolve_author(None, None, None, None, None), "unknown");
        assert_eq!(resolve_author(s(""), s("  "), None, None, s("")), "unknown");
    }
}
