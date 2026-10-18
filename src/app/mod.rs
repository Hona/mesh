//! Commands: orchestration only.

pub(crate) mod cp;
pub(crate) mod exec;
pub(crate) mod mux;
pub(crate) mod rm;
pub(crate) mod serve;
mod skill;
pub(crate) mod status;
mod store;
pub(crate) mod sync;
pub(crate) mod up;
pub(crate) mod update;

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// An error and the exit code it maps to (SPEC §5.2).
pub(crate) struct Fail(pub i32, pub String);

impl From<String> for Fail {
    fn from(s: String) -> Self {
        Fail(1, s)
    }
}

impl From<&str> for Fail {
    fn from(s: &str) -> Self {
        Fail(1, s.into())
    }
}

pub(crate) type R<T> = Result<T, Fail>;

/// Windows: make our stdin/stdout/stderr non-inheritable at startup. A child started with
/// inheritance gets every inheritable handle, so otherwise a caller's pipe travels down the
/// chain (update stage 1 → stage 2 → detached mux) and the caller never sees EOF.
pub(crate) fn isolate_std_handles() {
    #[cfg(windows)]
    crate::host::win::std_handles_private();
}

pub(crate) fn set_verbose(on: bool) {
    crate::host::VERBOSE.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// No console of our own (meshw, mcp): children get no windows; a windowless run with no
/// stdout/stderr (Task Scheduler) logs to `<state>/log/<name>.log`.
pub(crate) fn background(name: &str, header: &str) {
    crate::host::process::set_background();
    #[cfg(windows)]
    if crate::host::win::std_handles_missing()
        && let Some(f) = store::log_file(name, header)
    {
        use std::os::windows::io::IntoRawHandle;
        crate::host::win::set_std_handles(f.into_raw_handle().cast());
    }
    #[cfg(unix)]
    let _ = (name, header);
}

/// Cached devices (for the MCP `instructions` line).
pub(crate) fn devices() -> Vec<crate::model::Device> {
    store::all_devices()
}
