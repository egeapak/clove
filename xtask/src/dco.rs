//! `cargo xtask check-dco <base> <head>`: the Developer Certificate of Origin
//! check (CONTRIBUTING.md) that `.github/workflows/dco.yml` runs.
//!
//! Every non-merge commit in `<base>..<head>` must carry a `Signed-off-by:`
//! trailer whose email is the commit's author email (what `git commit -s`
//! writes). Each commit without one is reported as a GitHub Actions `::error::`
//! line, and the command fails.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, ensure, Context};

/// One commit of the range, as the check needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub author_email: String,
    pub subject: String,
    /// The values of the commit's `Signed-off-by` trailers, one per line.
    pub signoffs: String,
}

/// Whether one of `signoffs` (trailer values such as `Name <email>`, one per
/// line) names `author_email`. The email compare ignores case, like the shell
/// check that this replaces (`grep -iF "<email>"`).
pub fn signed_off_by_author(author_email: &str, signoffs: &str) -> bool {
    let wanted = format!("<{}>", author_email.to_lowercase());
    signoffs
        .lines()
        .any(|line| line.to_lowercase().contains(&wanted))
}

/// The commits of `base..head` that have no sign-off by their author.
pub fn unsigned(commits: &[Commit]) -> Vec<&Commit> {
    commits
        .iter()
        .filter(|c| !signed_off_by_author(&c.author_email, &c.signoffs))
        .collect()
}

fn git(repo: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .context("run git")?;
    ensure!(
        out.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8(out.stdout)?)
}

/// Read the non-merge commits of `base..head` in `repo`.
pub fn commits(repo: &Path, base: &str, head: &str) -> anyhow::Result<Vec<Commit>> {
    let range = format!("{base}..{head}");
    let shas = git(repo, &["rev-list", "--no-merges", &range])?;
    let mut commits = Vec::new();
    for sha in shas.lines().filter(|l| !l.is_empty()) {
        // NUL-separated: a subject or trailer cannot contain a NUL byte.
        let show = git(
            repo,
            &[
                "show",
                "-s",
                "--format=%ae%x00%s%x00%(trailers:key=Signed-off-by,valueonly)",
                sha,
            ],
        )?;
        let mut fields = show.splitn(3, '\0');
        let (Some(author_email), Some(subject), Some(signoffs)) =
            (fields.next(), fields.next(), fields.next())
        else {
            bail!("git show printed an unexpected format for {sha}: {show:?}");
        };
        commits.push(Commit {
            sha: sha.to_owned(),
            author_email: author_email.to_owned(),
            subject: subject.to_owned(),
            signoffs: signoffs.to_owned(),
        });
    }
    Ok(commits)
}

/// `cargo xtask check-dco <base> <head>`, in the current directory's repo.
pub fn check(args: Vec<String>) -> anyhow::Result<()> {
    let [base, head] = args.as_slice() else {
        bail!("usage: cargo xtask check-dco <base-ref> <head-ref>");
    };
    let commits = commits(Path::new("."), base, head)?;
    let missing = unsigned(&commits);
    for c in &missing {
        println!(
            "::error::commit {} ({}) has no Signed-off-by for its author <{}>",
            c.sha, c.subject, c.author_email
        );
    }
    if !missing.is_empty() {
        eprintln!(
            "{} of {} commit(s) lack a DCO sign-off. Add one with:",
            missing.len(),
            commits.len()
        );
        eprintln!("  git rebase --signoff {base}");
        eprintln!("then force-push your branch. See CONTRIBUTING.md.");
        std::process::exit(1);
    }
    println!("ok: all {} commit(s) are signed off", commits.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signoff_by_the_author_passes_in_any_case() {
        assert!(signed_off_by_author(
            "Dev@Example.com",
            "Someone Else <other@example.com>\nDev <dev@example.COM>\n"
        ));
    }

    #[test]
    fn a_signoff_by_someone_else_or_none_fails() {
        assert!(!signed_off_by_author(
            "dev@example.com",
            "Other <other@example.com>\n"
        ));
        assert!(!signed_off_by_author("dev@example.com", ""));
        // A bare address without the angle brackets is not a sign-off line.
        assert!(!signed_off_by_author(
            "dev@example.com",
            "dev@example.com\n"
        ));
    }

    fn run(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Dev")
            .env("GIT_AUTHOR_EMAIL", "dev@example.com")
            .env("GIT_COMMITTER_NAME", "Dev")
            .env("GIT_COMMITTER_EMAIL", "dev@example.com")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// The range read from a real repository: the unsigned commit is found,
    /// the signed one and the merge commit are not reported.
    #[test]
    fn the_check_reads_a_real_range_and_skips_merges() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run(repo, &["init", "-q", "-b", "main"]);
        run(repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let base = git(repo, &["rev-parse", "HEAD"]).unwrap();
        let base = base.trim();
        run(
            repo,
            &["commit", "-q", "--allow-empty", "-s", "-m", "signed"],
        );
        run(repo, &["commit", "-q", "--allow-empty", "-m", "not signed"]);
        run(repo, &["checkout", "-q", "-b", "side", base]);
        run(repo, &["commit", "-q", "--allow-empty", "-s", "-m", "side"]);
        run(repo, &["checkout", "-q", "main"]);
        run(
            repo,
            &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
        );

        let commits = commits(repo, base, "HEAD").unwrap();
        let subjects: Vec<_> = commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(commits.len(), 3, "{subjects:?}");
        assert!(!subjects.contains(&"merge side"), "{subjects:?}");
        let missing: Vec<_> = unsigned(&commits)
            .iter()
            .map(|c| c.subject.as_str())
            .collect();
        assert_eq!(missing, ["not signed"]);
    }
}
