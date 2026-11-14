//! The crates.io publish order, derived from the workspace itself.
//!
//! crates.io accepts a crate only when every crate it depends on is already
//! there, so the order is a topological order of the publishable workspace
//! crates. It used to be a hand-kept list in three places; one of them lost a
//! crate. `scripts/release/publish.sh` now asks `cargo xtask publish-order`.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use anyhow::{bail, ensure, Context};
use serde_json::Value;

/// Published last: `clove plugin install` resolves the plugin crates by name,
/// so they must be on the registry before the CLI that looks for them. No
/// dependency edge says so.
const LAST: &str = "clove-cli";

/// Print the publish order, one crate per line.
pub fn print_order() -> anyhow::Result<()> {
    for name in order(&metadata()?)? {
        println!("{name}");
    }
    Ok(())
}

/// `cargo metadata --no-deps` for the workspace this xtask belongs to.
pub fn metadata() -> anyhow::Result<Value> {
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/../Cargo.toml");
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
        ])
        .arg(manifest)
        .output()
        .context("running cargo metadata")?;
    ensure!(
        output.status.success(),
        "cargo metadata failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).context("parsing cargo metadata")
}

/// The publishable workspace crates in an order crates.io accepts: every crate
/// after all the workspace crates it depends on, [`LAST`] at the end, and
/// otherwise by name, so the result does not depend on the order of the input.
///
/// Dev-dependencies count when they name a version: `cargo publish` strips a
/// path-only dev-dependency but must resolve a versioned one on the registry.
pub fn order(metadata: &Value) -> anyhow::Result<Vec<String>> {
    let packages = metadata["packages"]
        .as_array()
        .context("cargo metadata has no `packages`")?;
    // `publish = false` is `[]` in the metadata; `null` means "any registry".
    let publishable = |package: &Value| package["publish"].as_array().is_none_or(|r| !r.is_empty());
    let name_of = |package: &Value| -> anyhow::Result<String> {
        Ok(package["name"]
            .as_str()
            .context("a package has no name")?
            .to_owned())
    };

    let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for package in packages.iter().filter(|p| publishable(p)) {
        deps.insert(name_of(package)?, BTreeSet::new());
    }
    for package in packages.iter().filter(|p| publishable(p)) {
        let name = name_of(package)?;
        for dep in package["dependencies"].as_array().into_iter().flatten() {
            let dep_name = dep["name"].as_str().unwrap_or_default();
            if dep_name == name || dep["path"].is_null() {
                continue;
            }
            let dev = dep["kind"].as_str() == Some("dev");
            if dev && dep["req"].as_str() == Some("*") {
                continue;
            }
            if deps.contains_key(dep_name) {
                deps.get_mut(&name)
                    .expect("inserted above")
                    .insert(dep_name.to_owned());
            } else if !dev {
                bail!(
                    "{name} depends on {dep_name}, which is not published: \
                     crates.io would reject {name}"
                );
            }
        }
    }
    ensure!(
        deps.contains_key(LAST),
        "guard broken: {LAST} is not a publishable workspace crate"
    );

    let mut order = Vec::with_capacity(deps.len());
    while order.len() < deps.len() {
        let ready = |name: &&String| {
            !order.contains(*name) && deps[*name].iter().all(|dep| order.contains(dep))
        };
        // Hold `LAST` back while anything else can go.
        let next = deps
            .keys()
            .filter(ready)
            .find(|name| name.as_str() != LAST)
            .or_else(|| deps.keys().find(ready));
        match next {
            Some(name) => order.push(name.clone()),
            None => {
                let stuck: Vec<_> = deps.keys().filter(|n| !order.contains(n)).collect();
                bail!("dependency cycle among the publishable crates: {stuck:?}");
            }
        }
    }
    ensure!(
        order.last().map(String::as_str) == Some(LAST),
        "{LAST} must be published last, but another crate depends on it: {order:?}"
    );
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn package(name: &str, publish: bool, deps: &[(&str, &str, &str)]) -> Value {
        let deps: Vec<Value> = deps
            .iter()
            .map(|(dep, kind, req)| {
                json!({
                    "name": dep,
                    "kind": if *kind == "normal" { Value::Null } else { json!(kind) },
                    "req": req,
                    "path": if dep.starts_with("clove") { json!(format!("/w/{dep}")) } else { Value::Null },
                })
            })
            .collect();
        json!({
            "name": name,
            "publish": if publish { Value::Null } else { json!([]) },
            "dependencies": deps,
        })
    }

    fn workspace(packages: Vec<Value>) -> Value {
        json!({ "packages": packages })
    }

    #[test]
    fn dependencies_come_first_and_the_cli_last() {
        // Listed in reverse on purpose: the input order must not matter.
        let metadata = workspace(vec![
            package("clove-cli", true, &[("clove-core", "normal", "^1")]),
            package("clove-sync-github", true, &[("clove-core", "normal", "^1")]),
            package(
                "clove-core",
                true,
                &[("clove-types", "normal", "^1"), ("serde", "normal", "^1")],
            ),
            package("clove-types", true, &[]),
            package("xtask", false, &[("clove-core", "normal", "^1")]),
        ]);
        assert_eq!(
            order(&metadata).unwrap(),
            [
                "clove-types",
                "clove-core",
                "clove-sync-github",
                "clove-cli"
            ]
        );
    }

    #[test]
    fn a_versioned_dev_dependency_orders_and_a_path_only_one_does_not() {
        let versioned = workspace(vec![
            package("clove-cli", true, &[]),
            package("clove-a", true, &[("clove-z", "dev", "^1")]),
            package("clove-z", true, &[]),
        ]);
        assert_eq!(
            order(&versioned).unwrap(),
            ["clove-z", "clove-a", "clove-cli"]
        );
        let path_only = workspace(vec![
            package("clove-cli", true, &[]),
            package("clove-a", true, &[("clove-z", "dev", "*")]),
            package("clove-z", true, &[("clove-a", "normal", "^1")]),
        ]);
        assert_eq!(
            order(&path_only).unwrap(),
            ["clove-a", "clove-z", "clove-cli"]
        );
    }

    #[test]
    fn a_dependency_on_an_unpublished_crate_is_an_error() {
        let metadata = workspace(vec![
            package("clove-cli", true, &[("clove-plugin-echo", "normal", "^1")]),
            package("clove-plugin-echo", false, &[]),
        ]);
        let err = order(&metadata).unwrap_err().to_string();
        assert!(err.contains("clove-plugin-echo"), "{err}");
    }

    #[test]
    fn a_cycle_is_an_error() {
        let metadata = workspace(vec![
            package("clove-cli", true, &[]),
            package("clove-a", true, &[("clove-b", "normal", "^1")]),
            package("clove-b", true, &[("clove-a", "normal", "^1")]),
        ]);
        assert!(order(&metadata).unwrap_err().to_string().contains("cycle"));
    }

    /// The real workspace: every publishable crate is in the order once, after
    /// its dependencies. This is the test that a new crate cannot be forgotten.
    #[test]
    fn the_workspace_order_is_complete_and_valid() {
        let metadata = metadata().unwrap();
        let order = order(&metadata).unwrap();
        let position = |name: &str| order.iter().position(|n| n == name);
        for package in metadata["packages"].as_array().unwrap() {
            let name = package["name"].as_str().unwrap();
            if package["publish"].as_array().is_some_and(Vec::is_empty) {
                assert_eq!(position(name), None, "{name} is `publish = false`");
                continue;
            }
            let at = position(name).unwrap_or_else(|| panic!("{name} is missing from {order:?}"));
            for dep in package["dependencies"].as_array().unwrap() {
                if dep["kind"].is_null() && !dep["path"].is_null() {
                    let dep_name = dep["name"].as_str().unwrap();
                    assert!(
                        position(dep_name).is_some_and(|dep_at| dep_at < at),
                        "{dep_name} must be published before {name}: {order:?}"
                    );
                }
            }
        }
        assert_eq!(order.last().map(String::as_str), Some("clove-cli"));
        assert_eq!(
            order.iter().collect::<BTreeSet<_>>().len(),
            order.len(),
            "no crate twice: {order:?}"
        );
    }
}
