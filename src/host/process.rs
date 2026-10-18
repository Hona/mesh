//! Processes: spawning (windowless when in the background), whole-tree kill, pid lookups.
//! OS APIs only, no helper processes.

use std::ffi::OsStr;
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

static BACKGROUND: AtomicBool = AtomicBool::new(false);

/// No console of our own (meshw, mux, mcp): children must not open console windows.
pub(crate) fn set_background() {
    BACKGROUND.store(true, Ordering::Relaxed);
}

#[cfg(windows)]
fn base_flags() -> u32 {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if BACKGROUND.load(Ordering::Relaxed) {
        CREATE_NO_WINDOW
    } else {
        0
    }
}

/// `Command::new`, plus CREATE_NO_WINDOW on Windows when running in the background.
pub(crate) fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(base_flags());
    }
    c
}

/// Run `cmd` (stdin null) for at most `limit`: (success, stdout, stderr), or None if it couldn't
/// start or was killed at the limit. Never waits on a grandchild still holding the pipes.
pub(crate) fn output_within(
    cmd: &mut Command,
    limit: std::time::Duration,
) -> Option<(bool, Vec<u8>, Vec<u8>)> {
    use std::io::Read;
    use std::time::{Duration, Instant};
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let reader = |s: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut b = Vec::new();
            if let Some(mut s) = s {
                let _ = s.read_to_end(&mut b);
            }
            let _ = tx.send(b);
        });
        rx
    };
    let out = reader(child.stdout.take().map(|s| Box::new(s) as _));
    let err = reader(child.stderr.take().map(|s| Box::new(s) as _));
    let t0 = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                let grace = Duration::from_millis(300);
                return Some((
                    st.success(),
                    out.recv_timeout(grace).unwrap_or_default(),
                    err.recv_timeout(grace).unwrap_or_default(),
                ));
            }
            Ok(None) if t0.elapsed() < limit => std::thread::sleep(Duration::from_millis(15)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Kills a spawned process and everything it started.
pub(crate) struct Killer {
    #[cfg(windows)]
    job: super::win::Handle,
    #[cfg(unix)]
    pgid: i32,
}

// The job handle is only used through thread-safe kernel calls.
unsafe impl Send for Killer {}
unsafe impl Sync for Killer {}

#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct ExtendedLimit {
    per_process_user_time: i64,
    per_job_user_time: i64,
    limit_flags: u32,
    min_working_set: usize,
    max_working_set: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
    io: [u64; 6],
    process_memory: usize,
    job_memory: usize,
    peak_process_memory: usize,
    peak_job_memory: usize,
}

#[cfg(windows)]
mod job {
    use super::super::win::Handle;
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn CreateJobObjectW(sa: *mut c_void, name: *const u16) -> Handle;
        pub fn SetInformationJobObject(job: Handle, class: u32, info: *mut c_void, len: u32)
        -> i32;
        pub fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        pub fn TerminateJobObject(job: Handle, code: u32) -> i32;
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        pub fn NtResumeProcess(process: Handle) -> i32;
    }
}

/// Spawn `cmd` so the whole tree can be killed: a kill-on-close job object on Windows
/// (assigned while suspended, so no grandchild escapes), a new process group on unix.
#[cfg(windows)]
pub(crate) fn spawn_tree(cmd: &mut Command) -> io::Result<(Child, Killer)> {
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    unsafe {
        let job = job::CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let killer = Killer { job };
        // KILL_ON_JOB_CLOSE (cleared by `release` on a normal exit) | BREAKAWAY_OK (a process
        // that asks to leave, like a detached mux, may)
        let mut info = ExtendedLimit {
            limit_flags: 0x2000 | 0x800,
            ..Default::default()
        };
        job::SetInformationJobObject(
            job,
            9,
            (&mut info as *mut ExtendedLimit).cast(),
            size_of::<ExtendedLimit>() as u32,
        );
        cmd.creation_flags(base_flags() | 0x4); // CREATE_SUSPENDED
        let child = cmd.spawn()?;
        let h = child.as_raw_handle();
        job::AssignProcessToJobObject(job, h);
        job::NtResumeProcess(h);
        Ok((child, killer))
    }
}

#[cfg(unix)]
pub(crate) fn spawn_tree(cmd: &mut Command) -> io::Result<(Child, Killer)> {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
    let child = cmd.spawn()?;
    let pgid = child.id() as i32;
    Ok((child, Killer { pgid }))
}

impl Killer {
    /// The command exited by itself: leave what it started running (SPEC §5.5). Windows: the
    /// job stops being kill-on-close, so closing it later kills nothing. Unix: nothing to do,
    /// the group is only ever signalled by `kill`.
    pub(crate) fn release(&self) {
        #[cfg(windows)]
        unsafe {
            let mut info = ExtendedLimit {
                limit_flags: 0x800,
                ..Default::default()
            };
            job::SetInformationJobObject(
                self.job,
                9,
                (&mut info as *mut ExtendedLimit).cast(),
                size_of::<ExtendedLimit>() as u32,
            );
        }
    }

    pub(crate) fn kill(&self) {
        #[cfg(windows)]
        unsafe {
            job::TerminateJobObject(self.job, 1);
        }
        #[cfg(unix)]
        unsafe {
            unsafe extern "C" {
                fn kill(pid: i32, sig: i32) -> i32;
            }
            kill(-self.pgid, 9);
        }
    }
}

#[cfg(windows)]
impl Drop for Killer {
    fn drop(&mut self) {
        super::win::close(self.job);
    }
}

/// Start a long-lived background process detached from this console/session.
pub(crate) fn spawn_detached(
    exe: &std::path::Path,
    args: &[&str],
    log: std::fs::File,
) -> io::Result<()> {
    let mut c = Command::new(exe);
    c.args(args)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        super::win::std_handles_private();
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP, and leave the caller's job if allowed
        // (e.g. a tool runner that kills its job when the call ends).
        if c.creation_flags(0x8 | 0x200 | 0x0100_0000).spawn().is_ok() {
            return Ok(());
        }
        c.creation_flags(0x8 | 0x200);
    }
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        unsafe extern "C" {
            fn setsid() -> i32;
        }
        c.pre_exec(|| {
            setsid();
            Ok(())
        });
    }
    c.spawn().map(drop)
}

/// Executable path of a live process; None if the pid is gone.
#[cfg(windows)]
pub(crate) fn pid_exe(pid: u32) -> Option<PathBuf> {
    use std::ffi::{OsString, c_void};
    use std::os::windows::ffi::OsStringExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn GetExitCodeProcess(h: *mut c_void, code: *mut u32) -> i32;
        fn QueryFullProcessImageNameW(
            h: *mut c_void,
            flags: u32,
            buf: *mut u16,
            len: *mut u32,
        ) -> i32;
        fn CloseHandle(h: *mut c_void) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut code = 0;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0
            && code == STILL_ACTIVE
            && QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) != 0;
        CloseHandle(h);
        ok.then(|| PathBuf::from(OsString::from_wide(&buf[..len as usize])))
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn pid_exe(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(target_os = "macos")]
pub(crate) fn pid_exe(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    unsafe extern "C" {
        fn proc_pidpath(pid: i32, buf: *mut std::ffi::c_void, size: u32) -> i32;
    }
    let mut buf = [0u8; 4096];
    let n = unsafe { proc_pidpath(pid as i32, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..n as usize])))
}

/// Parent pid recorded for `pid` (the parent itself may be gone; unix reparents orphans).
#[cfg(windows)]
pub(crate) fn parent(pid: u32) -> Option<u32> {
    use std::ffi::c_void;
    #[repr(C)]
    struct Entry {
        size: u32,
        usage: u32,
        pid: u32,
        heap: usize,
        module: u32,
        threads: u32,
        parent: u32,
        priority: i32,
        flags: u32,
        exe: [u16; 260],
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut c_void;
        fn Process32FirstW(snap: *mut c_void, e: *mut Entry) -> i32;
        fn Process32NextW(snap: *mut c_void, e: *mut Entry) -> i32;
        fn CloseHandle(h: *mut c_void) -> i32;
    }
    unsafe {
        let snap = CreateToolhelp32Snapshot(0x2, 0); // TH32CS_SNAPPROCESS
        if snap.is_null() || snap as isize == -1 {
            return None;
        }
        let mut e: Entry = std::mem::zeroed();
        e.size = size_of::<Entry>() as u32;
        let mut found = None;
        let mut ok = Process32FirstW(snap, &mut e) != 0;
        while ok {
            if e.pid == pid {
                found = Some(e.parent);
                break;
            }
            ok = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
        found
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn parent(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // "pid (comm) state ppid …": comm may contain spaces and parentheses
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[cfg(target_os = "macos")]
pub(crate) fn parent(pid: u32) -> Option<u32> {
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buf: *mut std::ffi::c_void,
            size: i32,
        ) -> i32;
    }
    // struct proc_bsdinfo (PROC_PIDTBSDINFO = 3): 136 bytes, pbi_ppid is the 5th u32
    let mut buf = [0u32; 34];
    let n = unsafe { proc_pidinfo(pid as i32, 3, 0, buf.as_mut_ptr().cast(), 136) };
    (n == 136).then_some(buf[4])
}

/// Member of BUILTIN\Administrators, elevated or not (what sshd's
/// `Match Group administrators` sees). A UAC-filtered token still lists the
/// group as deny-only, so this reads the token's groups rather than asking
/// "am I elevated" (CheckTokenMembership / IsUserAnAdmin).
#[cfg(windows)]
pub(crate) fn is_admin() -> bool {
    use std::ffi::c_void;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
        fn GetTokenInformation(
            token: *mut c_void,
            class: u32,
            info: *mut c_void,
            len: u32,
            ret: *mut u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn CloseHandle(h: *mut c_void) -> i32;
    }
    #[repr(C)]
    struct SidAndAttributes {
        sid: *const u8,
        _attributes: u32,
    }
    const TOKEN_QUERY: u32 = 8;
    const TOKEN_GROUPS: u32 = 2;
    // S-1-5-32-544: revision 1, 2 sub-authorities, authority 5, sub-authorities 32 and 544 (LE).
    const ADMINS: [u8; 16] = [1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 0x20, 2, 0, 0];
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut len = 0u32;
        GetTokenInformation(token, TOKEN_GROUPS, std::ptr::null_mut(), 0, &mut len);
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        let ok =
            GetTokenInformation(token, TOKEN_GROUPS, buf.as_mut_ptr().cast(), len, &mut len) != 0;
        CloseHandle(token);
        if !ok {
            return false;
        }
        // TOKEN_GROUPS { u32 count; SID_AND_ATTRIBUTES groups[] } (array is pointer-aligned)
        let base = buf.as_ptr().cast::<u8>();
        let count = *base.cast::<u32>() as usize;
        let groups = base.add(size_of::<usize>()).cast::<SidAndAttributes>();
        (0..count).any(|i| {
            let sid = (*groups.add(i)).sid;
            std::slice::from_raw_parts(sid, 2) == [1, 2]
                && std::slice::from_raw_parts(sid, 16) == ADMINS
        })
    }
}

#[cfg(not(windows))]
pub(crate) fn is_admin() -> bool {
    false
}
