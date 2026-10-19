//! This machine: paths, processes, admin check, opencode discovery, local socket,
//! scheduling, install.

pub(crate) mod install;
pub(crate) mod ipc;
pub(crate) mod opencode;
pub(crate) mod paths;
pub(crate) mod process;
pub(crate) mod schedule;
#[cfg(windows)]
pub(crate) mod win;

use std::sync::atomic::AtomicBool;

/// `n` random bytes from the OS, as hex.
pub(crate) fn random_hex(n: usize) -> Option<String> {
    let mut b = vec![0u8; n];
    #[cfg(windows)]
    {
        #[link(name = "advapi32")]
        unsafe extern "system" {
            // RtlGenRandom
            fn SystemFunction036(buf: *mut u8, len: u32) -> u8;
        }
        if unsafe { SystemFunction036(b.as_mut_ptr(), n as u32) } == 0 {
            return None;
        }
    }
    #[cfg(unix)]
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom")
            .ok()?
            .read_exact(&mut b)
            .ok()?;
    }
    Some(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Write a file only this user can read (unix 0600; Windows: inheritance cut, this user only).
pub(crate) fn write_secret(p: &std::path::Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    let err = |e: std::io::Error| format!("{}: {e}", p.display());
    let _ = std::fs::remove_file(p);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(p)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(err)?;
    #[cfg(windows)]
    {
        let sid = win::user_sid().ok_or("can't read the user SID")?;
        let ok = process::command("icacls")
            .arg(p)
            .args(["/inheritance:r", "/grant:r", &format!("*{sid}:F")])
            .output()
            .is_ok_and(|o| o.status.success());
        if !ok {
            return Err(format!("{}: icacls failed", p.display()));
        }
    }
    Ok(())
}

pub(crate) static VERBOSE: AtomicBool = AtomicBool::new(false);

/// eprintln! only with `-v`.
macro_rules! vlog {
    ($($t:tt)*) => {
        if $crate::host::VERBOSE.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!($($t)*);
        }
    };
}
pub(crate) use vlog;
