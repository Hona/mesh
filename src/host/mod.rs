//! This machine: paths, processes, admin check, opencode discovery, local socket,
//! scheduling, install.

pub(crate) mod install;
pub(crate) mod ipc;
pub(crate) mod opencode;
pub(crate) mod paths;
pub(crate) mod process;
pub(crate) mod schedule;
#[cfg(windows)]
pub(crate) mod win;

use std::sync::atomic::AtomicBool;

pub(crate) static VERBOSE: AtomicBool = AtomicBool::new(false);

/// eprintln! only with `-v`.
macro_rules! vlog {
    ($($t:tt)*) => {
        if $crate::host::VERBOSE.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!($($t)*);
        }
    };
}
pub(crate) use vlog;
