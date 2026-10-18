//! The only place that runs `tailscale`.

use crate::host::{paths, vlog};
use crate::model::Peer;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::process::Command;

#[derive(Deserialize)]
struct Node {
    #[serde(rename = "HostName", default)]
    host_name: String,
    #[serde(rename = "DNSName", default)]
    dns_name: String,
    #[serde(rename = "TailscaleIPs", default)]
    ips: Option<Vec<String>>,
    #[serde(rename = "Online", default)]
    online: bool,
}

#[derive(Deserialize)]
struct Status {
    #[serde(rename = "BackendState", default)]
    backend_state: String,
    #[serde(rename = "Self")]
    me: Option<Node>,
    #[serde(rename = "Peer")]
    peer: Option<HashMap<String, Node>>,
}

/// Address + online state for each host found, in one `tailscale status` call.
/// A host matches a node's MagicDNS label first, then its HostName (case-insensitive).
pub(crate) fn peers(hosts: &[&str]) -> Result<BTreeMap<String, Peer>, String> {
    let bin = paths::find("tailscale").ok_or("tailscale not found")?;
    vlog!("{} status --json", bin.display());
    let out = Command::new(&bin)
        .args(["status", "--json"])
        .output()
        .map_err(|e| format!("tailscale: {e}"))?;
    let s: Status = serde_json::from_slice(&out.stdout).map_err(|_| {
        format!(
            "tailscale status: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
    })?;
    let running = s.backend_state == "Running";
    let mut nodes: Vec<Node> = s.peer.unwrap_or_default().into_values().collect();
    nodes.extend(s.me);
    let label = |n: &Node| n.dns_name.split('.').next().unwrap_or("").to_string();
    let mut found = BTreeMap::new();
    for host in hosts {
        let node = nodes
            .iter()
            .find(|n| label(n).eq_ignore_ascii_case(host))
            .or_else(|| {
                nodes
                    .iter()
                    .find(|n| n.host_name.eq_ignore_ascii_case(host))
            });
        if let Some(n) = node {
            let address = n.ips.iter().flatten().find(|ip| ip.contains('.')).cloned();
            found.insert(
                host.to_string(),
                Peer {
                    address,
                    online: running && n.online,
                },
            );
        }
    }
    Ok(found)
}
