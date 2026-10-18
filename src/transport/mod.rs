//! How commands reach devices. SSH is the only transport.

mod ssh;

pub(crate) use ssh::{authorize, write_client_files};
