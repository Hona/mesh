//! Windows API calls (FFI, no crates).

use std::ffi::{OsStr, OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::time::Duration;

pub(crate) type Handle = *mut c_void;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CloseHandle(h: Handle) -> i32;
    fn GetCurrentProcess() -> Handle;
    fn WaitForSingleObject(h: Handle, ms: u32) -> u32;
    fn GetExitCodeProcess(h: Handle, code: *mut u32) -> i32;
    fn GetStdHandle(id: u32) -> Handle;
    fn SetStdHandle(id: u32, h: Handle) -> i32;
    fn LocalFree(p: *mut c_void) -> *mut c_void;
    fn GetLastError() -> u32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegGetValueW(
        key: Handle,
        subkey: *const u16,
        value: *const u16,
        flags: u32,
        ty: *mut u32,
        data: *mut c_void,
        len: *mut u32,
    ) -> i32;
    fn RegSetKeyValueW(
        key: Handle,
        subkey: *const u16,
        value: *const u16,
        ty: u32,
        data: *const c_void,
        len: u32,
    ) -> i32;
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn GetTokenInformation(
        token: Handle,
        class: u32,
        info: *mut c_void,
        len: u32,
        ret: *mut u32,
    ) -> i32;
    fn ConvertSidToStringSidW(sid: *mut c_void, out: *mut *mut u16) -> i32;
    fn OpenSCManagerW(machine: *const u16, db: *const u16, access: u32) -> Handle;
    fn OpenServiceW(scm: Handle, name: *const u16, access: u32) -> Handle;
    fn QueryServiceStatus(svc: Handle, status: *mut [u32; 7]) -> i32;
    fn CloseServiceHandle(h: Handle) -> i32;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn SendMessageTimeoutW(
        hwnd: Handle,
        msg: u32,
        wparam: usize,
        lparam: isize,
        flags: u32,
        timeout: u32,
        result: *mut usize,
    ) -> isize;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteExW(info: *mut ShellExecuteInfo) -> i32;
}

pub(crate) fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    OsString::from_wide(&buf[..end])
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn close(h: Handle) {
    unsafe { CloseHandle(h) };
}

pub(crate) fn last_error() -> u32 {
    unsafe { GetLastError() }
}

pub(crate) const HKCU: usize = 0x8000_0001;
pub(crate) const REG_EXPAND_SZ: u32 = 2;

fn hkey(root: usize) -> Handle {
    root as u32 as i32 as isize as Handle
}

/// A string value, unexpanded: (type, value).
pub(crate) fn reg_get(root: usize, subkey: &str, value: &str) -> Option<(u32, String)> {
    const FLAGS: u32 = 0x2 | 0x4 | 0x1000_0000; // RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND
    let (sk, v) = (wide(subkey), wide(value));
    let (mut ty, mut len) = (0u32, 0u32);
    unsafe {
        let r = RegGetValueW(
            hkey(root),
            sk.as_ptr(),
            v.as_ptr(),
            FLAGS,
            &mut ty,
            std::ptr::null_mut(),
            &mut len,
        );
        if r != 0 {
            return None;
        }
        let mut buf = vec![0u16; (len as usize).div_ceil(2)];
        let r = RegGetValueW(
            hkey(root),
            sk.as_ptr(),
            v.as_ptr(),
            FLAGS,
            &mut ty,
            buf.as_mut_ptr().cast(),
            &mut len,
        );
        (r == 0).then(|| (ty, from_wide(&buf)))
    }
}

pub(crate) fn reg_set(root: usize, subkey: &str, value: &str, ty: u32, data: &str) -> bool {
    let (sk, v, d) = (wide(subkey), wide(value), wide(data));
    let len = (d.len() * 2) as u32;
    unsafe {
        RegSetKeyValueW(
            hkey(root),
            sk.as_ptr(),
            v.as_ptr(),
            ty,
            d.as_ptr().cast(),
            len,
        ) == 0
    }
}

/// Tell running programs (Explorer, new terminals) that the user environment changed.
pub(crate) fn broadcast_env_change() {
    let env = wide("Environment");
    let mut res = 0usize;
    unsafe {
        SendMessageTimeoutW(
            0xffff as Handle,
            0x001A,
            0,
            env.as_ptr() as isize,
            0x2,
            3000,
            &mut res,
        );
    }
}

/// String SID of the user running this process, e.g. "S-1-5-21-…".
pub(crate) fn user_sid() -> Option<String> {
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), 0x8, &mut token) == 0 {
            return None;
        }
        let mut len = 0u32;
        GetTokenInformation(token, 1, std::ptr::null_mut(), 0, &mut len);
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        let ok = GetTokenInformation(token, 1, buf.as_mut_ptr().cast(), len, &mut len) != 0;
        CloseHandle(token);
        if !ok {
            return None;
        }
        // TOKEN_USER { SID_AND_ATTRIBUTES { PSID Sid; DWORD Attributes } }
        let sid = *buf.as_ptr().cast::<*mut c_void>();
        let mut s: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(sid, &mut s) == 0 {
            return None;
        }
        let n = (0..).take_while(|&i| *s.add(i) != 0).count();
        let out = from_wide(std::slice::from_raw_parts(s, n));
        LocalFree(s.cast());
        Some(out)
    }
}

/// Is a Windows service installed and running?
pub(crate) fn service_running(name: &str) -> bool {
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), 0x1);
        if scm.is_null() {
            return false;
        }
        let svc = OpenServiceW(scm, wide(name).as_ptr(), 0x4);
        let mut st = [0u32; 7];
        let running = !svc.is_null() && QueryServiceStatus(svc, &mut st) != 0 && st[1] == 4;
        if !svc.is_null() {
            CloseServiceHandle(svc);
        }
        CloseServiceHandle(scm);
        running
    }
}

/// Point stdout/stderr at `h` when this process has none (windowless meshw under Task Scheduler).
pub(crate) fn std_handles_missing() -> bool {
    let h = unsafe { GetStdHandle(-11i32 as u32) };
    h.is_null() || h as isize == -1
}

/// Stop our stdin/stdout/stderr from leaking into children through handle inheritance (a
/// detached child holding the caller's pipe keeps that pipe open). `Stdio::inherit` still works:
/// std duplicates the handle as inheritable for that one child.
pub(crate) fn std_handles_private() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetHandleInformation(h: Handle, mask: u32, flags: u32) -> i32;
    }
    for id in [-10i32, -11, -12] {
        let h = unsafe { GetStdHandle(id as u32) };
        if !h.is_null() && h as isize != -1 {
            unsafe { SetHandleInformation(h, 1, 0) }; // HANDLE_FLAG_INHERIT off
        }
    }
}

pub(crate) fn set_std_handles(h: Handle) {
    unsafe {
        SetStdHandle(-11i32 as u32, h);
        SetStdHandle(-12i32 as u32, h);
    }
}

#[repr(C)]
struct ShellExecuteInfo {
    size: u32,
    mask: u32,
    hwnd: Handle,
    verb: *const u16,
    file: *const u16,
    params: *const u16,
    dir: *const u16,
    show: i32,
    inst: Handle,
    id_list: *mut c_void,
    class: *const u16,
    class_key: Handle,
    hotkey: u32,
    icon: Handle,
    process: Handle,
}

pub(crate) enum Elevated {
    Exit(u32),
    Declined,
    NoAnswer,
    TimedOut,
    Failed(String),
}

/// Run `exe params` elevated (one UAC prompt) and wait for it; `tick` gets the elapsed time
/// every few seconds while it runs.
pub(crate) fn run_elevated(
    exe: &Path,
    params: &str,
    prompt: Duration,
    work: Duration,
    tick: &dyn Fn(Duration),
) -> Elevated {
    let (exe, params) = (wide(exe), wide(params));
    // ShellExecuteExW blocks until the prompt is answered; bound that with a thread.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let verb = wide("runas");
        let mut info = ShellExecuteInfo {
            size: size_of::<ShellExecuteInfo>() as u32,
            mask: 0x40 | 0x100 | 0x400, // NOCLOSEPROCESS | NOASYNC | FLAG_NO_UI
            hwnd: std::ptr::null_mut(),
            verb: verb.as_ptr(),
            file: exe.as_ptr(),
            params: params.as_ptr(),
            dir: std::ptr::null(),
            show: 0,
            inst: std::ptr::null_mut(),
            id_list: std::ptr::null_mut(),
            class: std::ptr::null(),
            class_key: std::ptr::null_mut(),
            hotkey: 0,
            icon: std::ptr::null_mut(),
            process: std::ptr::null_mut(),
        };
        let ok = unsafe { ShellExecuteExW(&mut info) } != 0;
        let _ = tx.send(if ok {
            Ok(info.process as usize)
        } else {
            Err(last_error())
        });
    });
    let process = match rx.recv_timeout(prompt) {
        Ok(Ok(p)) => p as Handle,
        Ok(Err(1223)) => return Elevated::Declined,
        Ok(Err(e)) => {
            return Elevated::Failed(std::io::Error::from_raw_os_error(e as i32).to_string());
        }
        Err(_) => return Elevated::NoAnswer,
    };
    if process.is_null() {
        return Elevated::Failed("no process handle".into());
    }
    let start = std::time::Instant::now();
    unsafe {
        while WaitForSingleObject(process, 5000) == 0x102 {
            if start.elapsed() > work {
                CloseHandle(process);
                return Elevated::TimedOut;
            }
            tick(start.elapsed());
        }
        let mut code = 0;
        GetExitCodeProcess(process, &mut code);
        CloseHandle(process);
        Elevated::Exit(code)
    }
}
