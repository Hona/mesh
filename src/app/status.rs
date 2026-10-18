//! `mesh status` (SPEC §5.4): repo cache + network for every device; without `--fast`, this
//! device is probed locally and every other online device runs `mesh status self --json`
//! over the mux, all in parallel. No registry calls.

use super::exec::{self, Input};
use super::{Fail, R, VERSION, skill, store};
use crate::host::opencode;
use crate::model::{DeviceStatus, Envelope, Opencode, Status};
use crate::network;
use std::collections::HashMap;

/// What a remote `mesh status self --json` said about itself.
struct Probe {
    reachable: bool,
    latency: Option<u64>,
    opencode: Option<Opencode>,
    mesh: Option<String>,
}

fn probe_remote(me: &str, devices: &[crate::model::Device]) -> HashMap<String, Probe> {
    if devices.is_empty() {
        return HashMap::new();
    }
    let argv = ["mesh", "status", "self", "--json"]
        .map(String::from)
        .to_vec();
    let env = Envelope {
        argv: Some(argv),
        timeout: Some(20.0),
        ..Default::default()
    };
    let results = exec::capture(me, devices, &env, Input::Empty, 1 << 20);
    results
        .into_iter()
        .map(|r| {
            let v: serde_json::Value =
                serde_json::from_str(r.stdout.as_deref().unwrap_or("")).unwrap_or_default();
            let d = &v["devices"][0];
            let opencode = d["opencode"].as_object().map(|o| Opencode {
                version: o.get("version").and_then(|x| x.as_str()).map(String::from),
                busy: o.get("busy").and_then(|x| x.as_u64()),
                service: o.get("service").and_then(|x| x.as_bool()).unwrap_or(false),
            });
            let ok = r.exit == Some(0);
            let probe = Probe {
                reachable: ok,
                latency: r.ms.filter(|_| ok),
                opencode,
                mesh: d["mesh"].as_str().map(String::from),
            };
            (r.device, probe)
        })
        .collect()
}

pub(crate) fn run(target: Option<&str>, exclude: Option<&str>, fast: bool) -> R<Status> {
    if store::read_index().is_empty() {
        store::read_state()?;
        return Err(Fail(
            1,
            "no local copy of the mesh yet: run `mesh sync`".into(),
        ));
    }
    let (me, mut devices) = exec::targets(target.unwrap_or("all"), exclude)?;
    let mut warnings = Vec::new();
    devices.sort_by_key(|d| d.name != me);
    let hosts: Vec<&str> = devices.iter().map(|d| d.host.as_str()).collect();
    let probe_self = !fast && devices.iter().any(|d| d.name == me);
    let (peers, mut local) = std::thread::scope(|s| {
        let local = probe_self.then(|| s.spawn(opencode::probe));
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
    let online = |host: &str| {
        peers
            .as_ref()
            .map(|p| p.get(host).is_some_and(|p| p.online))
    };
    let remote: Vec<_> = devices
        .iter()
        .filter(|d| !fast && d.name != me && online(&d.host) == Some(true))
        .cloned()
        .collect();
    let mut probes = probe_remote(&me, &remote);

    let rows = devices
        .into_iter()
        .map(|d| {
            let is_me = d.name == me;
            let probe = probes.remove(&d.name);
            DeviceStatus {
                online: online(&d.host),
                reachable: if is_me {
                    probe_self.then_some(true)
                } else {
                    probe.as_ref().map(|p| p.reachable)
                },
                latency_ms: probe.as_ref().and_then(|p| p.latency),
                opencode: if is_me {
                    local.take()
                } else {
                    probe.as_ref().and_then(|p| p.opencode.clone())
                },
                mesh: if is_me {
                    Some(VERSION.to_string())
                } else {
                    probe.and_then(|p| p.mesh)
                },
                me: is_me,
                name: d.name,
                os: d.os,
                arch: d.arch,
                shell: d.shell,
                user: d.user,
            }
        })
        .collect();
    if skill::customised() {
        warnings.push(
            "the mesh skill (skills/mesh/SKILL.md) was customised; mesh won't overwrite it".into(),
        );
    }
    let state = store::read_state()?;
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
