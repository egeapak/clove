//! Stdout writes that treat a closed reader as the end of the output.
//!
//! `println!` panics when the reader has gone away (`clove ls | head -1`), and
//! the release profile's `panic = "abort"` turns that into a SIGABRT. The host
//! and every plugin print through [`outln!`](crate::outln) / [`out!`](crate::out)
//! instead, which exit 0 on a broken pipe the way `head`-friendly CLIs do.
//! `clippy::print_stdout` is denied in those crates so a stray `println!` can't
//! reintroduce the panic.

use std::io::{self, Write};

/// Write `args` to stdout, exiting 0 if the reader has closed the pipe.
#[doc(hidden)]
pub fn write_fmt(args: std::fmt::Arguments<'_>) {
    if let Err(err) = io::stdout().lock().write_fmt(args) {
        exit_if_broken_pipe(&err);
        panic!("failed printing to stdout: {err}");
    }
}

/// Exit 0 if `err` is a broken pipe — for callers that write to a locked stdout
/// handle themselves (streaming exports) and would otherwise report it as an
/// error.
pub fn exit_if_broken_pipe(err: &io::Error) {
    if err.kind() == io::ErrorKind::BrokenPipe {
        std::process::exit(0);
    }
}

/// `println!` that exits 0 instead of panicking when stdout's reader is gone.
#[macro_export]
macro_rules! outln {
    () => {
        $crate::stdout::write_fmt(format_args!("\n"))
    };
    ($($arg:tt)*) => {
        $crate::stdout::write_fmt(format_args!("{}\n", format_args!($($arg)*)))
    };
}

/// `print!` that exits 0 instead of panicking when stdout's reader is gone.
#[macro_export]
macro_rules! out {
    ($($arg:tt)*) => {
        $crate::stdout::write_fmt(format_args!($($arg)*))
    };
}
