//! How commands reach devices. SSH is the only transport; persistent links go through the mux.

pub(crate) mod frame;
pub(crate) mod mux;
pub(crate) mod ssh;

pub(crate) use ssh::{authorize, write_client_files};
