//! OS APIs only, no helper processes.

use std::path::PathBuf;

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
