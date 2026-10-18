//! Commands: orchestration only.

pub(crate) mod status;
mod store;
pub(crate) mod sync;

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// An error and the exit code it maps to (SPEC §5.2).
pub(crate) struct Fail(pub i32, pub String);

impl From<String> for Fail {
    fn from(s: String) -> Self {
        Fail(1, s)
    }
}

pub(crate) type R<T> = Result<T, Fail>;

pub(crate) fn set_verbose(on: bool) {
    crate::host::VERBOSE.store(on, std::sync::atomic::Ordering::Relaxed);
}
