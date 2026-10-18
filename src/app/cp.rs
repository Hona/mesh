//! `mesh cp` (SPEC §5.7): scp with the mesh SSH config; device→device runs on the source.

use super::exec::{self, Event, Input};
use super::{Fail, R};
use crate::model::{Envelope, valid_name};
use crate::transport::ssh;
use std::io::Write;

/// `<device>:<path>` -> Some((device, path)); anything else is a local path.
/// Single letters are never devices, so `C:\x` stays local.
fn side(s: &str) -> Option<(&str, &str)> {
    s.split_once(':').filter(|(d, _)| valid_name(d))
}

pub(crate) fn run(recursive: bool, src: &str, dst: &str) -> R<i32> {
    let (from, to) = (side(src), side(dst));
    for (dev, _) in [from, to].into_iter().flatten() {
        exec::targets(dev, None)?;
    }
    match (from, to) {
        (None, None) => Err(Fail(2, "cp: one side must be <device>:<path>".into())),
        (Some((dev, path)), Some(_)) => {
            let (me, devices) = exec::targets(dev, None)?;
            let mut argv = vec!["mesh".to_string(), "cp".to_string()];
            if recursive {
                argv.push("-r".into());
            }
            argv.extend([path.to_string(), dst.to_string()]);
            let env = Envelope {
                argv: Some(argv),
                ..Default::default()
            };
            let mut code = 255;
            exec::run(&me, &devices, &env, Input::Empty, &mut |ev| match ev {
                Event::Out(_, b) => {
                    let _ = std::io::stdout().write_all(&b);
                }
                Event::Err(_, b) => {
                    let _ = std::io::stderr().write_all(&b);
                }
                Event::Exit(_, info, _) => {
                    if let Some(e) = info.error {
                        eprintln!("mesh: {dev}: {e}");
                    }
                    code = info.code.unwrap_or(255);
                }
            });
            Ok(code)
        }
        _ => {
            let status = ssh::scp(recursive, src, dst)?
                .status()
                .map_err(|e| format!("scp: {e}"))?;
            Ok(status.code().unwrap_or(255))
        }
    }
}
