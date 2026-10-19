//! `mesh update` (SPEC §5.9) and converging to `mesh.json` `pin` from sync (§5.6 step 3).

use super::exec::{self, Input};
use super::skill::{self, Publish};
use super::{Fail, R, VERSION, store, sync};
use crate::host::{install, opencode, paths, process};
use crate::model::{Envelope, RegistryConfig, State, Step, cmp_versions};
use crate::registry;
use std::fs;

/// Where mesh releases are published.
const TOOL_REPO: &str = "Hona/mesh";

pub(crate) struct Opts {
    pub version: Option<String>,
    pub check: bool,
    /// Hidden: the second half of an update, run by the freshly installed binary.
    pub resume: bool,
}

fn tool() -> Result<registry::Registry, String> {
    let cfg = RegistryConfig {
        kind: "github".into(),
        repo: TOOL_REPO.into(),
        path: String::new(),
        branch: "main".into(),
    };
    registry::open(&cfg)
}

/// (release asset, installed file name) for this OS/arch (SPEC §10).
fn assets() -> Vec<(String, &'static str)> {
    let (os, arch) = paths::os_arch();
    let ext = if cfg!(windows) { ".exe" } else { "" };
    install::names()
        .iter()
        .map(|n| {
            (
                format!("{}-{os}-{arch}{ext}", n.trim_end_matches(".exe")),
                *n,
            )
        })
        .collect()
}

/// Newest release; prereleases count only when this build is one.
fn latest(tool: &registry::Registry) -> Result<String, String> {
    let pre = VERSION.contains('-');
    tool.releases()?
        .into_iter()
        .filter(|r| pre || !r.prerelease)
        .map(|r| r.tag.trim_start_matches('v').to_string())
        .max_by(|a, b| cmp_versions(a, b))
        .ok_or_else(|| format!("no releases in {TOOL_REPO}: pass --version"))
}

/// Download `version` for this OS/arch, check it against `SHA256SUMS`, swap it in at the
/// install path (a running binary is renamed aside to `*.old`).
fn install_version(version: &str) -> Result<(), String> {
    let dir = paths::state_dir().join("update").join(version);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let assets = assets();
    let mut names: Vec<String> = assets.iter().map(|(a, _)| a.clone()).collect();
    names.push("SHA256SUMS".into());
    tool()?.download(&format!("v{version}"), &names, &dir)?;
    let sums =
        fs::read_to_string(dir.join("SHA256SUMS")).map_err(|e| format!("SHA256SUMS: {e}"))?;
    for (asset, _) in &assets {
        let want = sums
            .lines()
            .find_map(|l| {
                let (h, n) = l.split_once(char::is_whitespace)?;
                (n.trim().trim_start_matches('*') == asset).then(|| h.to_lowercase())
            })
            .ok_or_else(|| format!("{asset} isn't listed in SHA256SUMS"))?;
        let bytes = fs::read(dir.join(asset)).map_err(|e| format!("{asset}: {e}"))?;
        if sha256(&bytes) != want {
            return Err(format!("{asset}: SHA256 mismatch, not installed"));
        }
    }
    for (asset, name) in &assets {
        let src = dir.join(asset);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&src, fs::Permissions::from_mode(0o755));
        }
        install::replace(&src, &install::dir().join(name))?;
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// sync step 3: move this device to the mesh's pin. No repo writes, no fan-out. Ok(a hint when
/// the MCP server couldn't be restarted).
pub(crate) fn converge(pin: &str) -> Result<Option<String>, String> {
    install_version(pin.trim_start_matches('v'))?;
    Ok(opencode::reconnect_mcp("mesh").err())
}

pub(crate) fn run(o: &Opts, step: &dyn Fn(Step)) -> R<i32> {
    let say = |status: &'static str, name: &str, detail: String| {
        step(Step {
            status,
            name: name.into(),
            detail,
        })
    };
    install::clean_old();
    let state = store::read_state()?;
    let target = match &o.version {
        Some(v) => v.trim().trim_start_matches('v').to_string(),
        None => latest(&tool().map_err(|e| Fail(3, e))?)?,
    };
    if o.check {
        let pin = store::mesh().pin.unwrap_or_else(|| "-".into());
        say(
            "info",
            "mesh",
            format!("this device {VERSION} · mesh.json pin {pin} · target {target}"),
        );
        let tool = tool().map_err(|e| Fail(3, e))?;
        let mut rel: Vec<String> = tool
            .releases()?
            .into_iter()
            .map(|r| {
                let v = r.tag.trim_start_matches('v').to_string();
                if r.prerelease {
                    format!("{v} (prerelease)")
                } else {
                    v
                }
            })
            .collect();
        rel.truncate(6);
        say("info", "releases", rel.join(" · "));
        return Ok(0);
    }
    // Run by another device's fan-out (inside `mesh exec`): `gh` can't log in here, and the
    // caller already pushed the pin, so this device's own sync job converges to it (§5.6 step 3).
    if std::env::var_os("MESH_FROM").is_some() && !o.resume {
        let rep = sync::run(false, false)?;
        for line in rep.changes.iter().chain(&rep.applied) {
            say("ok", "sync", line.clone());
        }
        for w in &rep.warnings {
            say("warn", "sync", w.clone());
        }
        let now = install::installed_version().unwrap_or_else(|| "?".into());
        if now != target {
            return Err(Fail(
                1,
                format!("still mesh {now} after its sync job (pin {target})"),
            ));
        }
        say(
            "ok",
            "binary",
            format!("now {target} (via this device's sync job)"),
        );
        return Ok(0);
    }
    if target != VERSION {
        if o.resume {
            return Err(Fail(
                1,
                format!("expected mesh {target} after the swap, but this is {VERSION}"),
            ));
        }
        install_version(&target)?;
        say(
            "ok",
            "binary",
            format!("{VERSION} → {target} (SHA256 verified)"),
        );
        // The rest runs in the new binary: it carries the new skill and logic.
        let mesh = install::dir().join(install::names()[0]);
        let status = process::command(&mesh)
            .args(["update", "--version", &target, "--resume"])
            .status()
            .map_err(|e| format!("{}: {e}", mesh.display()))?;
        return Ok(status.code().unwrap_or(1));
    }
    if o.resume {
        exec::quit_old_muxes();
    } else {
        say("ok", "binary", format!("already {VERSION}"));
    }
    let mut failed = false;
    // Run on another device by its `mesh update` fan-out: just this device.
    if std::env::var_os("MESH_FROM").is_none() {
        publish(&state, &target, &say)?;
        failed = fan_out(&state.device, &target, &say);
        // The fan-out's links run the devices' pre-update mesh; the next command redials.
        exec::stop_mux();
    }
    // Only a swapped binary needs the MCP server restarted.
    if o.resume {
        match opencode::reconnect_mcp("mesh") {
            Ok(0) => say("info", "mcp", "opencode service not running".into()),
            Ok(n) => say(
                "ok",
                "mcp",
                format!("\"mesh\" restarted on {target} ({n} opencode locations)"),
            ),
            Err(e) => say("warn", "mcp", e),
        }
    }
    Ok(if failed { 1 } else { 0 })
}

/// Pin + built-in skill in one explicit commit (SPEC §5.9 step 2, §9 guards).
fn publish(state: &State, version: &str, say: &dyn Fn(&'static str, &str, String)) -> R<()> {
    let reg = registry::open(&state.registry).map_err(|e| Fail(3, e))?;
    sync::fetch(&reg)?.ok_or("the registry has no mesh.json")?;
    let text =
        fs::read_to_string(store::cached("mesh.json")).map_err(|e| format!("mesh.json: {e}"))?;
    let mut mesh: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("mesh.json: {e}"))?;
    let obj = mesh.as_object_mut().ok_or("mesh.json isn't an object")?;
    let mut files = Vec::new();
    let pin_changed = obj.get("pin").and_then(|p| p.as_str()) != Some(version);
    if pin_changed {
        obj.insert("pin".into(), version.into());
        let text = serde_json::to_string_pretty(&mesh).map_err(|e| e.to_string())? + "\n";
        files.push(("mesh.json".to_string(), Some(text.into_bytes())));
    }
    let skill = match skill::publish() {
        Publish::Write(text) => {
            files.push(("skills/mesh/SKILL.md".to_string(), Some(text.into_bytes())));
            ("ok", format!("skills/mesh/SKILL.md updated to {version}"))
        }
        Publish::UpToDate => ("ok", "skills/mesh/SKILL.md already current".into()),
        Publish::Newer(v) => (
            "warn",
            format!("the mesh's skill is from mesh {v}: not downgraded"),
        ),
        Publish::Customised => ("warn", "the mesh's skill was customised: left alone".into()),
    };
    if !files.is_empty() {
        reg.commit(&files, &format!("mesh update: {version}"))?;
        // so `status` here sees the new pin right away
        sync::fetch(&reg)?;
    }
    say(
        "ok",
        "pin",
        if pin_changed {
            format!("mesh.json pin {version} pushed")
        } else {
            format!("already pinned to {version}")
        },
    );
    say(skill.0, "skill", skill.1);
    Ok(())
}

/// `mesh exec all mesh update --version <v>` on the other devices; offline ones follow the pin later.
fn fan_out(me: &str, version: &str, say: &dyn Fn(&'static str, &str, String)) -> bool {
    let devices: Vec<_> = store::all_devices()
        .into_iter()
        .filter(|d| d.name != me)
        .collect();
    if devices.is_empty() {
        return false;
    }
    let argv = ["mesh", "update", "--version", version]
        .map(String::from)
        .to_vec();
    let env = Envelope {
        argv: Some(argv),
        timeout: Some(300.0),
        ..Default::default()
    };
    let mut failed = false;
    for r in exec::capture(me, &devices, &env, Input::Empty, 64 << 10) {
        match (r.exit, r.error.as_deref()) {
            (_, Some("offline")) => say(
                "info",
                &r.device,
                format!("offline: moves to {version} on its next sync"),
            ),
            (Some(0), _) => say("ok", &r.device, format!("updated to {version}")),
            _ => {
                failed = true;
                let why = r.error.or(r.stderr).unwrap_or_default();
                say(
                    "fail",
                    &r.device,
                    why.trim().lines().last().unwrap_or("failed").to_string(),
                );
            }
        }
    }
    failed
}

/// SHA-256 (FIPS 180-4), hex.
fn sha256(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}
