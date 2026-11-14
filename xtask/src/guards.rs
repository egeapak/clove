//! Dependency-tree guards that CI runs (`.github/workflows/ci.yml`,
//! "feature combos + release size"). Each guard runs `cargo tree` and hands its
//! output to a pure function, which the unit tests below cover. Each one has a
//! positive control: it fails when `cargo tree` stops printing a row that must
//! be there, so it cannot pass on empty or unrelated output.

use std::process::{Command, Output};

use anyhow::{bail, ensure, Context};

/// The crates that form the permissively licensed plugin SDK (CLAUDE.md,
/// "Licensing"). `xtask/tests/license_files.rs` pins their license files.
pub const SDK_CRATES: [&str; 3] = ["clove-types", "clove-plugin", "clove-plugin-echo"];

/// The license every SDK crate declares.
const SDK_LICENSE: &str = "MIT OR Apache-2.0";

/// Separates the package from its license in a `cargo tree --format` row. A
/// license expression contains spaces, so a space cannot do it.
const SEP: &str = " ||| ";

fn cargo_output(args: &[&str]) -> anyhow::Result<Output> {
    // The guards parse this output. CI sets `CARGO_TERM_COLOR=always`, which
    // makes `cargo tree` wrap the `(*)` marker in colour codes.
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(args)
        .env("CARGO_TERM_COLOR", "never")
        .output()
        .with_context(|| format!("running cargo {}", args.join(" ")))
}

fn stdout_of(args: &[&str]) -> anyhow::Result<String> {
    let output = cargo_output(args)?;
    ensure!(
        output.status.success(),
        "cargo {} failed:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The SDK crates must not depend on a copyleft crate: a plugin built on the
/// SDK alone may use any license. Checks each SDK crate on its own (a workspace
/// build unifies features and would hide the problem), with default features
/// and with all features, on every target, over normal, build **and dev**
/// edges. A crate that declares no `license` at all fails too: the guard cannot
/// tell what it is.
pub fn sdk_licenses() -> anyhow::Result<()> {
    let format = format!("{{p}}{SEP}{{l}}");
    let mut problems = Vec::new();
    for package in SDK_CRATES {
        for all_features in [false, true] {
            let mut args = vec![
                "tree",
                "-p",
                package,
                "-e",
                "normal,build,dev",
                "--target",
                "all",
                "--prefix",
                "none",
                "--format",
                &format,
            ];
            if all_features {
                args.push("--all-features");
            }
            let rows = stdout_of(&args)?;
            let features = if all_features {
                "all features"
            } else {
                "default features"
            };
            for problem in sdk_license_problems(&rows)? {
                problems.push(format!("{package} ({features}): {problem}"));
            }
        }
    }
    problems.sort();
    problems.dedup();
    if !problems.is_empty() {
        bail!(
            "the plugin SDK must not depend on a copyleft or unlicensed crate:\n  {}",
            problems.join("\n  ")
        );
    }
    println!("ok: no copyleft or unlicensed crate in the SDK dependency trees");
    Ok(())
}

/// The rows of one `cargo tree --prefix none --format '{p} ||| {l}'` run that
/// break the SDK rule. `Err` when the output is not what the guard expects (the
/// positive control), so a changed `cargo tree` cannot make the guard pass.
fn sdk_license_problems(rows: &str) -> anyhow::Result<Vec<String>> {
    let mut control = false;
    let mut problems = Vec::new();
    for row in rows.lines().filter(|row| !row.trim().is_empty()) {
        let Some((package, license)) = row.split_once(SEP) else {
            bail!("guard broken: unexpected `cargo tree` row `{row}`");
        };
        // `(*)` marks a subtree printed earlier.
        let (package, license) = (package.trim(), license.trim_end_matches("(*)").trim());
        if package.starts_with("clove-types v") && license == SDK_LICENSE {
            control = true;
        }
        if license.is_empty() {
            problems.push(format!("{package} declares no license"));
        } else if is_copyleft(license) {
            problems.push(format!("{package} is {license}"));
        }
    }
    ensure!(
        control,
        "guard broken: no `clove-types v… {SDK_LICENSE}` row in the `cargo tree` output"
    );
    Ok(problems)
}

/// Whether a license expression forces a GPL-family license (GPL, LGPL, AGPL)
/// on the user. `A OR B` does so only when both sides do, because the user
/// picks one; `A AND B` does when either side does. An expression this cannot
/// read counts as copyleft, so the guard fails closed.
fn is_copyleft(expression: &str) -> bool {
    // The pre-SPDX `MIT/Apache-2.0` form means OR.
    let spaced = expression
        .replace('(', " ( ")
        .replace(')', " ) ")
        .replace('/', " OR ");
    let tokens: Vec<&str> = spaced.split_whitespace().collect();
    let mut at = 0;
    match copyleft_or(&tokens, &mut at) {
        Some(copyleft) if at == tokens.len() => copyleft,
        _ => true,
    }
}

fn copyleft_or(tokens: &[&str], at: &mut usize) -> Option<bool> {
    let mut all = copyleft_and(tokens, at)?;
    while tokens
        .get(*at)
        .is_some_and(|t| t.eq_ignore_ascii_case("OR"))
    {
        *at += 1;
        all &= copyleft_and(tokens, at)?;
    }
    Some(all)
}

fn copyleft_and(tokens: &[&str], at: &mut usize) -> Option<bool> {
    let mut any = copyleft_term(tokens, at)?;
    while tokens
        .get(*at)
        .is_some_and(|t| t.eq_ignore_ascii_case("AND"))
    {
        *at += 1;
        any |= copyleft_term(tokens, at)?;
    }
    Some(any)
}

fn copyleft_term(tokens: &[&str], at: &mut usize) -> Option<bool> {
    let token = *tokens.get(*at)?;
    *at += 1;
    if token == "(" {
        let inner = copyleft_or(tokens, at)?;
        (tokens.get(*at) == Some(&")")).then(|| *at += 1)?;
        return Some(inner);
    }
    if token == ")"
        || ["OR", "AND", "WITH"]
            .iter()
            .any(|k| token.eq_ignore_ascii_case(k))
    {
        return None;
    }
    // `GPL-2.0 WITH Classpath-exception-2.0`: the exception does not change
    // the family.
    if tokens
        .get(*at)
        .is_some_and(|t| t.eq_ignore_ascii_case("WITH"))
    {
        tokens.get(*at + 1)?;
        *at += 2;
    }
    Some(token.to_ascii_uppercase().contains("GPL"))
}

/// `web` is off by default: a plain `cargo install clove-cli` or `cloved` must
/// not link the HTTP server or the embedded SPA.
pub fn no_web() -> anyhow::Result<()> {
    let tree = stdout_of(&["tree", "-p", "clove-cli", "-p", "cloved", "-e", "normal"])?;
    ensure!(
        !default_tree_has_web(&tree)?,
        "clove-web is in the default dependency tree of clove-cli/cloved"
    );
    println!("ok: the default clove-cli and cloved trees carry no clove-web");
    Ok(())
}

/// Whether a `cargo tree -p clove-cli -p cloved -e normal` output names
/// `clove-web`. `Err` when the two root packages are missing from it.
fn default_tree_has_web(tree: &str) -> anyhow::Result<bool> {
    for root in ["clove-cli v", "cloved v"] {
        ensure!(
            tree.lines().any(|line| line.starts_with(root)),
            "guard broken: no `{root}…` root in the `cargo tree` output"
        );
    }
    Ok(tree.lines().any(|line| {
        line.split_whitespace()
            .any(|word| word == "clove-web" || word.starts_with("clove-web/"))
    }))
}

/// The registry client (ureq -> rustls) must stay on `ring`. A rustls with
/// default features unifies the tree onto aws-lc-sys and triples the binary, so
/// the workspace pins `ring` (root `Cargo.toml`).
///
/// This asserts that aws-lc-sys is unreachable through **normal** edges.
/// `-e normal` does the scoping. `-p` is absent on purpose: it does not
/// restrict `-i`, and cargo ignores an unknown `-p` and widens to the whole
/// workspace. aws-lc-sys stays reachable through clove-cli's `jsonschema`
/// dev-dependency and stays in `Cargo.lock` through octocrab either way.
pub fn tls() -> anyhow::Result<()> {
    // `cargo tree` ignores a misspelled package and prints an unrelated tree;
    // `cargo pkgid` fails, so it catches a check that has stopped checking.
    ensure!(
        cargo_output(&["pkgid", "-p", "clove-cli"])?
            .status
            .success(),
        "guard broken: `clove-cli` is not a package in this workspace"
    );
    let ring = cargo_output(&["tree", "-e", "normal", "-i", "ring"])?;
    let aws = cargo_output(&["tree", "-e", "normal", "-i", "aws-lc-sys"])?;
    tls_verdict(
        &String::from_utf8_lossy(&ring.stdout),
        aws.status.code(),
        &String::from_utf8_lossy(&aws.stdout),
    )?;
    println!("ok: ring present, aws-lc-sys absent from all normal dependency edges");
    Ok(())
}

/// Judge the two inverse-tree runs. `cargo tree -i` exits 0 with nothing on
/// stdout when the package is in the lockfile but filtered out of this tree,
/// and 101 when it is absent from the lockfile. Both mean "not reachable"; any
/// other status means the check did not run.
fn tls_verdict(ring_tree: &str, aws_status: Option<i32>, aws_tree: &str) -> anyhow::Result<()> {
    // Positive control: without the registry client, "aws-lc-sys is absent"
    // would be true for the wrong reason.
    ensure!(
        ring_tree.lines().any(|line| line.starts_with("ring")),
        "positive control failed: ring is absent from the normal tree. Either the \
         registry client was removed, or this check is broken."
    );
    ensure!(
        matches!(aws_status, Some(0) | Some(101)),
        "cargo tree failed unexpectedly ({aws_status:?}); the result is not conclusive"
    );
    ensure!(
        aws_tree.trim().is_empty(),
        "aws-lc-sys is reachable through normal edges:\n{aws_tree}\nThe rustls `ring` pin \
         in the root Cargo.toml has been defeated by feature unification."
    );
    Ok(())
}

/// `guard-glibc <floor> <binary>...`: every glibc symbol version that a Linux
/// binary requires is at most `<floor>` (for example `2.35`). The release
/// workflow runs it on the packaged binaries, so a build on a newer image
/// fails there and not on a user's machine.
pub fn glibc(args: Vec<String>) -> anyhow::Result<()> {
    let mut args = args.into_iter();
    let floor_text = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: cargo xtask guard-glibc <floor> <binary>..."))?;
    let floor = glibc_version(&floor_text)
        .ok_or_else(|| anyhow::anyhow!("`{floor_text}` is not a glibc version such as 2.35"))?;
    let binaries: Vec<String> = args.collect();
    anyhow::ensure!(
        !binaries.is_empty(),
        "usage: cargo xtask guard-glibc <floor> <binary>..."
    );
    let mut failed = false;
    for binary in &binaries {
        let output = Command::new("objdump").args(["-T", binary]).output()?;
        anyhow::ensure!(
            output.status.success(),
            "objdump -T {binary} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let highest = highest_glibc(&String::from_utf8_lossy(&output.stdout))?;
        if highest > floor {
            failed = true;
            eprintln!(
                "FAIL: {binary} requires glibc {}.{}, above the floor {floor_text}",
                highest.0, highest.1
            );
        } else {
            println!("ok: {binary} requires glibc {}.{}", highest.0, highest.1);
        }
    }
    anyhow::ensure!(!failed, "a binary requires a newer glibc than {floor_text}");
    Ok(())
}

/// `2.35` or `2.2.5` as (major, minor). The third part does not matter for a
/// floor check.
fn glibc_version(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    parts
        .all(|part| part.parse::<u32>().is_ok())
        .then_some((major, minor))
}

/// The highest `GLIBC_x.y` version in `objdump -T` output. No such version is
/// an error: the binary is static or not a glibc binary, so the guard would
/// pass without checking anything.
fn highest_glibc(objdump: &str) -> anyhow::Result<(u32, u32)> {
    let mut highest = None;
    for token in objdump.split(|c: char| c.is_whitespace() || c == '(' || c == ')') {
        let Some(version) = token.strip_prefix("GLIBC_") else {
            continue;
        };
        // `GLIBC_PRIVATE` and `GLIBC_ABI_DT_RELR` carry no version number.
        let Some(version) = glibc_version(version) else {
            continue;
        };
        highest = highest.max(Some(version));
    }
    highest.ok_or_else(|| anyhow::anyhow!("the binary names no GLIBC_x.y symbol version"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highest_glibc_reads_the_newest_required_version() {
        let objdump = "\
0000000000000000      DF *UND*  0000000000000000 (GLIBC_2.2.5) write
0000000000000000      DF *UND*  0000000000000000 (GLIBC_2.34) pthread_create
0000000000000000      DF *UND*  0000000000000000 (GLIBC_2.9)  pipe2
0000000000000000      DO *UND*  0000000000000000 (GLIBC_ABI_DT_RELR) x
0000000000000000  w   DF *UND*  0000000000000000 (GLIBC_2.39) pidfd_spawnp
";
        // 2.39 > 2.9: the comparison is numeric, not textual.
        assert_eq!(highest_glibc(objdump).unwrap(), (2, 39));
        assert!(highest_glibc(objdump).unwrap() > glibc_version("2.35").unwrap());
        assert!(highest_glibc(objdump).unwrap() <= glibc_version("2.39").unwrap());
    }

    #[test]
    fn a_binary_without_glibc_versions_is_an_error() {
        assert!(highest_glibc("no dynamic symbols").is_err());
        assert!(highest_glibc("(GLIBC_PRIVATE) __libc_x").is_err());
    }

    #[test]
    fn glibc_version_rejects_text_that_is_not_a_version() {
        assert_eq!(glibc_version("2.35"), Some((2, 35)));
        assert_eq!(glibc_version("2.2.5"), Some((2, 2)));
        assert_eq!(glibc_version("2"), None);
        assert_eq!(glibc_version("latest"), None);
    }

    const CONTROL: &str = "clove-types v0.3.0 (/w/crates/clove-types) ||| MIT OR Apache-2.0\n";

    #[test]
    fn a_permissive_tree_passes() {
        let rows = format!(
            "{CONTROL}serde v1.0.0 ||| MIT OR Apache-2.0\n\
             serde_derive v1.0.0 (proc-macro) ||| MIT OR Apache-2.0 (*)\n"
        );
        assert!(sdk_license_problems(&rows).unwrap().is_empty());
    }

    #[test]
    fn a_copyleft_dependency_is_reported() {
        for license in [
            "AGPL-3.0-only",
            "GPL-2.0",
            "LGPL-2.1-or-later",
            "MIT AND gpl-3.0",
        ] {
            let rows = format!("{CONTROL}clove-core v0.3.0 (/w/crates/clove-core) ||| {license}\n");
            let problems = sdk_license_problems(&rows).unwrap();
            assert_eq!(problems.len(), 1, "{license}: {problems:?}");
            assert!(problems[0].contains("clove-core"), "{problems:?}");
        }
    }

    #[test]
    fn a_license_choice_is_copyleft_only_when_every_choice_is() {
        for free in [
            "MIT",
            "MIT OR Apache-2.0 OR LGPL-2.1-or-later",
            "MIT/Apache-2.0",
            "(MIT OR Apache-2.0) AND Unicode-3.0",
            "Apache-2.0 WITH LLVM-exception",
            "(GPL-3.0 AND MIT) OR ISC",
        ] {
            assert!(!is_copyleft(free), "{free}");
        }
        for copyleft in [
            "AGPL-3.0-only",
            "GPL-2.0 WITH Classpath-exception-2.0",
            "GPL-3.0 OR LGPL-2.1",
            "GPL-3.0 AND (MIT OR Apache-2.0)",
            "(MIT OR GPL-3.0) AND LGPL-2.1",
            // Not a readable expression: fail closed.
            "MIT OR",
            "(MIT",
            "MIT Apache-2.0",
        ] {
            assert!(is_copyleft(copyleft), "{copyleft}");
        }
    }

    #[test]
    fn a_dependency_without_a_license_is_reported() {
        let rows = format!("{CONTROL}mystery v0.1.0 ||| \n");
        let problems = sdk_license_problems(&rows).unwrap();
        assert_eq!(problems, ["mystery v0.1.0 declares no license"]);
    }

    #[test]
    fn output_without_the_control_row_is_an_error() {
        assert!(sdk_license_problems("").is_err());
        assert!(sdk_license_problems("serde v1.0.0 ||| MIT\n").is_err());
        // An SDK crate that changed its own license is not the control row.
        assert!(sdk_license_problems("clove-types v0.3.0 ||| AGPL-3.0-only\n").is_err());
        // A row in another format means `cargo tree` changed.
        assert!(sdk_license_problems("clove-types v0.3.0 MIT OR Apache-2.0\n").is_err());
    }

    const ROOTS: &str = "clove-cli v0.3.0 (/w/crates/clove)\n├── anyhow v1.0.0\n\ncloved v0.3.0 (/w/crates/cloved)\n";

    #[test]
    fn a_default_tree_without_the_web_crate_passes() {
        let tree = format!("{ROOTS}└── clove-webhooks v1.0.0\n");
        assert!(!default_tree_has_web(&tree).unwrap());
    }

    #[test]
    fn the_web_crate_in_the_default_tree_is_found() {
        let tree = format!("{ROOTS}└── clove-web v0.3.0 (/w/crates/clove-web)\n");
        assert!(default_tree_has_web(&tree).unwrap());
    }

    #[test]
    fn a_tree_without_both_roots_is_an_error() {
        assert!(default_tree_has_web("").is_err());
        assert!(default_tree_has_web("clove-cli v0.3.0 (/w/crates/clove)\n").is_err());
    }

    #[test]
    fn ring_without_aws_lc_passes_for_both_absent_statuses() {
        let ring = "ring v0.17.0\n└── rustls v0.23.0\n";
        assert!(tls_verdict(ring, Some(0), "").is_ok());
        assert!(tls_verdict(ring, Some(101), "").is_ok());
    }

    #[test]
    fn the_tls_guard_fails_closed() {
        let ring = "ring v0.17.0\n";
        // No ring: the positive control.
        assert!(tls_verdict("", Some(0), "").is_err());
        // aws-lc-sys reachable.
        assert!(tls_verdict(ring, Some(0), "aws-lc-sys v0.30.0\n└── aws-lc-rs\n").is_err());
        // cargo tree did not run to a verdict.
        assert!(tls_verdict(ring, Some(1), "").is_err());
        assert!(tls_verdict(ring, None, "").is_err());
    }
}
