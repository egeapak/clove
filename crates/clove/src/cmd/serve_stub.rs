//! `clove serve` in a build without the `web` feature: the subcommand stays so
//! the error can say how to get the web UI.

use clove_types::CloveError;

use crate::cli::ServeArgs;
use crate::context::Ctx;

pub fn run(
    _ctx: &Ctx,
    _args: ServeArgs,
    _quiet: bool,
    _no_index: bool,
    _deep: bool,
) -> Result<(), CloveError> {
    Err(CloveError::Io {
        path: camino::Utf8PathBuf::from("."),
        source: std::io::Error::other(
            "this clove binary was built without the web UI: install with \
             `cargo install clove-cli --features web`, or use a release binary",
        ),
    })
}
