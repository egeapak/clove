//! Checking each call's project token against the project's
//! `.clove/daemon.token` (DESIGN §8.4).
//!
//! The token scopes automated clients per project: a call acts on a project
//! only if its caller could read that project's `.clove/`. The file is read
//! again on every call — it is a few dozen bytes — so whatever it holds now is
//! what counts, however it was rewritten.

use camino::Utf8Path;
use clove_core::daemon_token;
use clove_ipc::hub::codes;

use crate::slot::LoadError;

/// Accept `presented` only if it is the token in `clove_dir`. Blocking (a
/// small file read): call it off the async workers.
pub fn check(clove_dir: &Utf8Path, presented: &str) -> Result<(), LoadError> {
    let refused = |message: String| LoadError {
        code: codes::BAD_TOKEN,
        message,
    };
    let expected = daemon_token::read(clove_dir).map_err(|e| refused(e.to_string()))?;
    if daemon_token::matches(&expected, presented) {
        Ok(())
    } else {
        Err(refused(format!(
            "this call's token is not the one in {}; a client may act only on a \
             project whose .clove/ it can read",
            daemon_token::token_path(clove_dir)
        )))
    }
}
