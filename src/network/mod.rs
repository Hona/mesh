//! How devices are found and whether they're online. Tailscale is the only backend.

mod tailscale;

pub(crate) use tailscale::peers;
