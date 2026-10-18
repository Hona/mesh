//! `mesh status` (SPEC §5.4). Phase 1: repo cache + network for every device,
//! plus a local probe of this device. No registry calls.

use super::store;
use super::{Fail, R, VERSION};
use crate::host::opencode;
use crate::model::{DeviceStatus, Status, Target};
use crate::network;

pub(crate) fn run(target: Option<&str>, exclude: Option<&str>, fast: bool) -> R<Status> {
    let target = Target::parse(target.unwrap_or("all"), exclude).map_err(|e| Fail(2, e))?;
    let state = store::read_state()?;
    let index = store::read_index();
    if index.is_empty() {
        return Err(Fail(
            1,
            "no local copy of the mesh yet: run `mesh sync`".into(),
        ));
    }
    let mut warnings = Vec::new();
    let mut devices = store::devices(&index, &mut warnings);
    let me = state.device;
    let names = target.resolve(&devices, &me).map_err(|e| Fail(2, e))?;
    devices.retain(|d| names.contains(&d.name));
    devices.sort_by_key(|d| d.name != me);

    let probe = !fast && names.contains(&me);
    let hosts: Vec<&str> = devices.iter().map(|d| d.host.as_str()).collect();
    let (peers, mut local) = std::thread::scope(|s| {
        let local = probe.then(|| s.spawn(opencode::probe));
        (
            network::peers(&hosts),
            local.and_then(|h| h.join().ok().flatten()),
        )
    });
    let peers = match peers {
        Ok(p) => Some(p),
        Err(e) => {
            warnings.push(e);
            None
        }
    };

    let rows = devices
        .into_iter()
        .map(|d| {
            let is_me = d.name == me;
            DeviceStatus {
                online: peers
                    .as_ref()
                    .map(|p| p.get(&d.host).is_some_and(|p| p.online)),
                reachable: (is_me && probe).then_some(true),
                latency_ms: None,
                opencode: if is_me { local.take() } else { None },
                mesh: is_me.then(|| VERSION.to_string()),
                me: is_me,
                name: d.name,
                os: d.os,
                arch: d.arch,
                shell: d.shell,
                user: d.user,
            }
        })
        .collect();
    Ok(Status {
        me,
        devices: rows,
        synced: state.synced,
        synced_ago_secs: store::state_age_secs(),
        pin: store::mesh().pin,
        agents_backup: store::latest_agents_backup().map(|p| p.display().to_string()),
        warnings,
    })
}
