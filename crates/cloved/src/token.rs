//! Checking each call's project token against the project's
//! `.clove/daemon.token` (DESIGN §8.4).
//!
//! The token scopes automated clients per project: a call acts on a project
//! only if its caller could read that project's `.clove/`. The hub keeps each
//! project's token in memory and reads the file again whenever it changes.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::SystemTime;

use camino::{Utf8Path, Utf8PathBuf};
use clove_core::daemon_token;
use clove_ipc::hub::codes;

use crate::slot::LoadError;

/// What identifies one version of a token file.
#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    inode: u64,
}

impl Stamp {
    fn of(meta: &std::fs::Metadata) -> Stamp {
        Stamp {
            len: meta.len(),
            modified: meta.modified().ok(),
            #[cfg(unix)]
            inode: std::os::unix::fs::MetadataExt::ino(meta),
        }
    }
}

/// Every project's token as last read, keyed by its canonical `.clove/`.
#[derive(Default)]
pub struct Tokens(Mutex<HashMap<Utf8PathBuf, (Stamp, String)>>);

impl Tokens {
    /// Accept `presented` only if it is the token in `clove_dir`. Blocking
    /// (a stat, and a read when the file changed): call it off the async
    /// workers.
    pub fn check(&self, clove_dir: &Utf8Path, presented: &str) -> Result<(), LoadError> {
        let refused = |why: String| LoadError {
            code: codes::BAD_TOKEN,
            message: why,
        };
        let path = daemon_token::token_path(clove_dir);
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| refused(format!("the project has no daemon token ({path}: {e})")))?;
        if !meta.is_file() {
            return Err(refused(format!("{path} is not a token file")));
        }
        let stamp = Stamp::of(&meta);
        let mut cache = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let expected = match cache.get(clove_dir) {
            Some((seen, token)) if *seen == stamp => token.clone(),
            _ => {
                let token = daemon_token::read(clove_dir)
                    .map_err(|e| refused(format!("reading {path}: {e}")))?;
                cache.insert(clove_dir.to_owned(), (stamp, token.clone()));
                token
            }
        };
        if daemon_token::matches(&expected, presented) {
            Ok(())
        } else {
            Err(refused(format!(
                "this call's token is not the one in {path}; a client may act only on \
                 a project whose .clove/ it can read"
            )))
        }
    }
}
