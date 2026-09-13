//! `fake-cargo` — the test double `crates/clove/tests/plugin_install.rs` puts on
//! `PATH` as `cargo`, and *also* the "plugin" that double installs.
//!
//! # Why this is a compiled binary and not a shell script
//!
//! It used to be a `#!/bin/sh` script written at test time, which is why the
//! whole install suite carried `#![cfg(unix)]` — and why CI's `windows-latest`
//! leg silently skipped every one of those tests. Two Windows-only bugs shipped
//! through that hole (a path built without `EXE_SUFFIX`, and a suffixed name
//! reaching `cargo --bin`).
//!
//! A `.cmd`/`.bat` shim does not close it. `std::process::Command` on Windows
//! appends `.exe` to an extension-less program name rather than walking
//! `PATHEXT`, and both consumers here go through `Command`: clove spawns `cargo`,
//! and clove spawns the *installed plugin* to probe it. The second is
//! unconditional — whatever "cargo" writes into `<root>/bin` has to be a real
//! executable. So one compiled binary wears both hats.
//!
//! # The two hats
//!
//! 1. **As `cargo`**: `install` records its argv and copies itself to
//!    `<--root>/bin/<--bin><EXE_SUFFIX>`; `uninstall` deletes that file (the real
//!    cargo does, and a shim that did not made clove's "is it actually gone?"
//!    check untestable). Every other subcommand — notably `cargo metadata`, which
//!    the `--git` path needs — is passed through to the real cargo, or the shim
//!    would be answering questions it knows nothing about.
//! 2. **As the installed plugin**: answers `--clove-plugin-info` according to
//!    `$FAKE_PLUGIN_MODE`, which reaches it by plain inheritance (test harness →
//!    `clove` → probe), so no file plumbing is needed.
//!
//! Deliberately named without the `clove-` prefix: `target/debug/clove-fake-cargo`
//! would be enumerated as a real plugin by `plugin::list()` in every
//! `plugin list` test in the repo.

use std::io::Write;

/// Where the shim appends one line per faked invocation.
const LOG: &str = "FAKE_CARGO_LOG";
/// Absolute path to the real cargo, for pass-through.
const REAL: &str = "FAKE_CARGO_REAL";
/// A subcommand the shim should fail on, so the failure paths are reachable.
const FAIL_ON: &str = "FAKE_CARGO_FAIL_ON";
/// Which `--clove-plugin-info` answer to give when run as a plugin.
const MODE: &str = "FAKE_PLUGIN_MODE";

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    if argv.first().map(String::as_str) == Some("--clove-plugin-info") {
        print_plugin_info();
        return std::process::ExitCode::SUCCESS;
    }

    let subcommand = argv.first().map(String::as_str).unwrap_or("");
    if !matches!(subcommand, "install" | "uninstall") {
        return passthrough(&argv);
    }

    if let Ok(path) = std::env::var(LOG) {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{}", argv.join(" "));
        }
    }

    // A cargo that fails, so the "install failed" and "rollback failed" paths
    // are reachable from a test instead of only by reading the code.
    if std::env::var(FAIL_ON).as_deref() == Ok(subcommand) {
        eprintln!("error: fake cargo was told to fail on `{subcommand}`");
        return std::process::ExitCode::from(101);
    }

    match subcommand {
        "install" => do_install(&argv),
        "uninstall" => do_uninstall(&argv),
        _ => unreachable!(),
    }
}

/// The value of `--<name>` in `argv`, if present.
fn flag<'a>(argv: &'a [String], name: &str) -> Option<&'a str> {
    argv.iter()
        .position(|a| a == name)
        .and_then(|i| argv.get(i + 1))
        .map(String::as_str)
}

/// The positional after `--` — the package name, on both real argvs.
fn positional(argv: &[String]) -> Option<&str> {
    argv.iter()
        .position(|a| a == "--")
        .and_then(|i| argv.get(i + 1))
        .map(String::as_str)
}

/// Materialize the binary cargo was asked to install.
fn do_install(argv: &[String]) -> std::process::ExitCode {
    let (Some(root), Some(bin)) = (flag(argv, "--root"), flag(argv, "--bin")) else {
        eprintln!("error: fake cargo install needs --root and --bin");
        return std::process::ExitCode::from(101);
    };
    let dir = std::path::Path::new(root).join("bin");
    if std::fs::create_dir_all(&dir).is_err() {
        return std::process::ExitCode::from(101);
    }
    // The suffix is the whole point of compiling this: clove probes the path it
    // *predicts* cargo wrote, so the fixture has to write the path cargo really
    // would.
    let dest = dir.join(format!("{bin}{}", std::env::consts::EXE_SUFFIX));
    let Ok(me) = std::env::current_exe() else {
        return std::process::ExitCode::from(101);
    };
    // Windows refuses to overwrite a running image, and `--force` reinstalls
    // over an existing file, so remove first.
    let _ = std::fs::remove_file(&dest);
    if std::fs::copy(&me, &dest).is_err() {
        return std::process::ExitCode::from(101);
    }
    std::process::ExitCode::SUCCESS
}

/// Remove what a matching `install` wrote.
fn do_uninstall(argv: &[String]) -> std::process::ExitCode {
    let (Some(root), Some(package)) = (flag(argv, "--root"), positional(argv)) else {
        eprintln!("error: fake cargo uninstall needs --root and a package");
        return std::process::ExitCode::from(101);
    };
    let path = std::path::Path::new(root)
        .join("bin")
        .join(format!("{package}{}", std::env::consts::EXE_SUFFIX));
    let _ = std::fs::remove_file(path);
    std::process::ExitCode::SUCCESS
}

/// Hand everything else to the real cargo.
fn passthrough(argv: &[String]) -> std::process::ExitCode {
    let Ok(real) = std::env::var(REAL) else {
        eprintln!("error: {REAL} is not set, so `cargo {argv:?}` cannot be forwarded");
        return std::process::ExitCode::from(101);
    };
    match std::process::Command::new(real).args(argv).status() {
        Ok(status) => std::process::ExitCode::from(status.code().unwrap_or(1) as u8),
        Err(e) => {
            eprintln!("error: could not run the real cargo: {e}");
            std::process::ExitCode::from(101)
        }
    }
}

/// Answer the host's compatibility probe.
fn print_plugin_info() {
    let api = clove_plugin::CLOVE_PLUGIN_API;
    let (version, about, min, max) = match std::env::var(MODE).as_deref() {
        // Demands a newer clove: gate 3 must refuse and roll back.
        Ok("needs_newer") => ("9.0.0", "future", api + 1, api + 1),
        // Predates this clove: installs, with a warning.
        Ok("outdated") => ("0.0.1", "old", api.saturating_sub(1), api.saturating_sub(1)),
        // Answers nothing at all.
        Ok("no_info") => {
            std::process::exit(1);
        }
        _ => ("1.0.0", "ok", api, api),
    };
    println!(
        r#"{{"name":"p","version":"{version}","about":"{about}","provides":["sync:gitlab"],"clove_plugin_api":{api},"min_clove_plugin_api":{min},"max_clove_plugin_api":{max},"max_schema":1}}"#
    );
}
