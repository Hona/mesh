//! `mesh mux` (hidden): the local background mux (SPEC §6.5). Started on demand, detached;
//! exits after 10 minutes without clients.

use super::{Fail, R, VERSION, exec, store};
use crate::host::{ipc, process};
use crate::network;
use crate::transport::{mux, ssh};
use std::time::Duration;

pub(crate) fn run() -> R<i32> {
    process::set_background();
    let listener = ipc::bind(exec::BUILD).map_err(|e| Fail(1, format!("mux: {e}")))?;
    eprintln!(
        "mux {VERSION} build {} pid {}",
        exec::BUILD,
        std::process::id()
    );
    let dial: mux::Dial = Box::new(|dev: &str| {
        let d = store::all_devices()
            .into_iter()
            .find(|d| d.name == dev)
            .ok_or_else(|| format!("unknown device {dev}"))?;
        // Offline devices are reported at once, never waited on (SPEC §5.1).
        if let Ok(p) = network::peers(&[d.host.as_str()])
            && !p.get(&d.host).is_some_and(|p| p.online)
        {
            return Err("offline".into());
        }
        ssh::serve(&d)
    });
    mux::serve(listener, dial, Duration::from_secs(600), exec::hello());
    Ok(0)
}
