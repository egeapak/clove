//! cargo xtask — clove build/bench tooling.
//!
//! Commands (`bench-fixtures`, `bench-compare`, `test-all`) are implemented in
//! T-X03. Stub for now so the workspace builds.

fn main() -> anyhow::Result<()> {
    let cmd = std::env::args().nth(1);
    match cmd.as_deref() {
        Some(other) => {
            eprintln!("xtask: command `{other}` not yet implemented (see T-X03)");
            std::process::exit(1);
        }
        None => {
            eprintln!("usage: cargo xtask <bench-fixtures|bench-compare|test-all>");
            std::process::exit(1);
        }
    }
}
