//! Per-user, per-build local socket between mesh processes and the mux (SPEC §6.5):
//! a named pipe on Windows (current user only, no remote clients), a unix socket elsewhere.
//! `&Conn` implements Read + Write and can be used from several threads at once.

pub(crate) use imp::{Conn, Listener, bind, connect, endpoints};

#[cfg(windows)]
mod imp {
    use super::super::win::{self, Handle};
    use std::ffi::c_void;
    use std::io::{self, Read, Write};
    use std::sync::Mutex;

    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: Handle,
    }

    #[repr(C)]
    struct SecurityAttributes {
        len: u32,
        sd: *mut c_void,
        inherit: i32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateNamedPipeW(
            name: *const u16,
            open: u32,
            mode: u32,
            max: u32,
            out_size: u32,
            in_size: u32,
            timeout: u32,
            sa: *mut SecurityAttributes,
        ) -> Handle;
        fn ConnectNamedPipe(h: Handle, ov: *mut Overlapped) -> i32;
        fn WaitNamedPipeW(name: *const u16, ms: u32) -> i32;
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            sa: *mut c_void,
            disposition: u32,
            flags: u32,
            template: Handle,
        ) -> Handle;
        fn ReadFile(h: Handle, buf: *mut u8, len: u32, read: *mut u32, ov: *mut Overlapped) -> i32;
        fn WriteFile(
            h: Handle,
            buf: *const u8,
            len: u32,
            written: *mut u32,
            ov: *mut Overlapped,
        ) -> i32;
        fn GetOverlappedResult(h: Handle, ov: *mut Overlapped, n: *mut u32, wait: i32) -> i32;
        fn CreateEventW(sa: *mut c_void, manual: i32, initial: i32, name: *const u16) -> Handle;
        fn FindFirstFileW(name: *const u16, data: *mut FindData) -> Handle;
        fn FindNextFileW(h: Handle, data: *mut FindData) -> i32;
        fn FindClose(h: Handle) -> i32;
    }

    /// WIN32_FIND_DATAW
    #[repr(C)]
    struct FindData {
        attrs: u32,
        times: [u32; 6],
        size: [u32; 2],
        reserved: [u32; 2],
        name: [u16; 260],
        alt: [u16; 14],
    }

    /// Build tags of this user's running muxes ("" = from before per-build names).
    pub(crate) fn endpoints() -> Vec<String> {
        let Some(sid) = win::user_sid() else {
            return Vec::new();
        };
        let prefix = format!("opencode-mesh-{sid}");
        let mut out = Vec::new();
        let mut fd: FindData = unsafe { std::mem::zeroed() };
        let h = unsafe { FindFirstFileW(win::wide(r"\\.\pipe\*").as_ptr(), &mut fd) };
        if h as isize == -1 {
            return out;
        }
        loop {
            let end = fd
                .name
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(fd.name.len());
            let name = String::from_utf16_lossy(&fd.name[..end]);
            match name.strip_prefix(&prefix) {
                Some("") => out.push(String::new()),
                Some(rest) if rest.starts_with('-') => out.push(rest[1..].to_string()),
                _ => {}
            }
            if unsafe { FindNextFileW(h, &mut fd) } == 0 {
                break;
            }
        }
        unsafe { FindClose(h) };
        out.sort();
        out.dedup();
        out
    }
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl: *const u16,
            rev: u32,
            sd: *mut *mut c_void,
            len: *mut u32,
        ) -> i32;
    }

    const IO_PENDING: u32 = 997;
    const OVERLAPPED_FLAG: u32 = 0x4000_0000;

    fn name(tag: &str) -> io::Result<Vec<u16>> {
        let sid = win::user_sid().ok_or_else(|| io::Error::other("no user SID"))?;
        let suffix = if tag.is_empty() {
            String::new()
        } else {
            format!("-{tag}")
        };
        Ok(win::wide(format!(r"\\.\pipe\opencode-mesh-{sid}{suffix}")))
    }

    // One event per thread, reused for every overlapped call made on that thread.
    struct Event(Handle);
    impl Drop for Event {
        fn drop(&mut self) {
            win::close(self.0);
        }
    }
    thread_local! {
        static EVENT: Event = Event(unsafe { CreateEventW(std::ptr::null_mut(), 1, 0, std::ptr::null()) });
    }

    /// Run one overlapped call to completion. `start` returns the call's BOOL result.
    fn complete(h: Handle, start: impl FnOnce(*mut Overlapped) -> i32) -> io::Result<u32> {
        EVENT.with(|ev| unsafe {
            let mut ov = Overlapped {
                internal: 0,
                internal_high: 0,
                offset: 0,
                offset_high: 0,
                event: ev.0,
            };
            if start(&mut ov) == 0 {
                let e = win::last_error();
                if e != IO_PENDING {
                    return Err(io::Error::from_raw_os_error(e as i32));
                }
            }
            let mut n = 0u32;
            if GetOverlappedResult(h, &mut ov, &mut n, 1) == 0 {
                return Err(io::Error::from_raw_os_error(win::last_error() as i32));
            }
            Ok(n)
        })
    }

    pub(crate) struct Conn(Handle);
    unsafe impl Send for Conn {}
    unsafe impl Sync for Conn {}

    impl Drop for Conn {
        fn drop(&mut self) {
            win::close(self.0);
        }
    }

    impl Read for &Conn {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let len = buf.len().min(1 << 20) as u32;
            match complete(self.0, |ov| unsafe {
                ReadFile(self.0, buf.as_mut_ptr(), len, std::ptr::null_mut(), ov)
            }) {
                Ok(n) => Ok(n as usize),
                // broken pipe / pipe not connected / no data: the other side is gone
                Err(e) if matches!(e.raw_os_error(), Some(109 | 233 | 232)) => Ok(0),
                Err(e) => Err(e),
            }
        }
    }

    impl Write for &Conn {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let len = buf.len().min(1 << 20) as u32;
            complete(self.0, |ov| unsafe {
                WriteFile(self.0, buf.as_ptr(), len, std::ptr::null_mut(), ov)
            })
            .map(|n| n as usize)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    pub(crate) struct Listener {
        name: Vec<u16>,
        sa: SecurityAttributes,
        next: Mutex<Option<Conn>>,
    }
    unsafe impl Send for Listener {}
    unsafe impl Sync for Listener {}

    impl Listener {
        fn instance(&self, first: bool) -> io::Result<Conn> {
            const FIRST: u32 = 0x0008_0000;
            const REJECT_REMOTE: u32 = 0x8;
            let open = 0x3 | OVERLAPPED_FLAG | if first { FIRST } else { 0 };
            let sa = &self.sa as *const SecurityAttributes as *mut SecurityAttributes;
            let h = unsafe {
                CreateNamedPipeW(
                    self.name.as_ptr(),
                    open,
                    REJECT_REMOTE,
                    255,
                    65536,
                    65536,
                    0,
                    sa,
                )
            };
            if h as isize == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(Conn(h))
        }

        /// Wait for the next client.
        pub(crate) fn accept(&self) -> io::Result<Conn> {
            let pending = self.next.lock().unwrap_or_else(|e| e.into_inner()).take();
            let conn = match pending {
                Some(c) => c,
                None => self.instance(false)?,
            };
            match complete(conn.0, |ov| unsafe { ConnectNamedPipe(conn.0, ov) }) {
                Ok(_) => {}
                Err(e) if e.raw_os_error() == Some(535) => {} // client connected before we waited
                Err(e) => return Err(e),
            }
            *self.next.lock().unwrap_or_else(|e| e.into_inner()) = self.instance(false).ok();
            Ok(conn)
        }
    }

    /// Claim the pipe name (fails if a mux already runs for this user).
    pub(crate) fn bind(tag: &str) -> io::Result<Listener> {
        let name = name(tag)?;
        let sid = win::user_sid().unwrap_or_default();
        let sddl = win::wide(format!("D:P(A;;GA;;;{sid})"));
        let mut sd = std::ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut sd,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let sa = SecurityAttributes {
            len: size_of::<SecurityAttributes>() as u32,
            sd,
            inherit: 0,
        };
        let l = Listener {
            name,
            sa,
            next: Mutex::new(None),
        };
        let first = l.instance(true)?;
        *l.next.lock().unwrap_or_else(|e| e.into_inner()) = Some(first);
        Ok(l)
    }

    /// Connect to the running mux; None if there isn't one.
    pub(crate) fn connect(tag: &str) -> io::Result<Option<Conn>> {
        let name = name(tag)?;
        for _ in 0..20 {
            // GENERIC_READ|WRITE, OPEN_EXISTING, overlapped, identify-only impersonation
            let flags = OVERLAPPED_FLAG | 0x0010_0000 | 0x0001_0000;
            let h = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    0xC000_0000,
                    0,
                    std::ptr::null_mut(),
                    3,
                    flags,
                    std::ptr::null_mut(),
                )
            };
            if h as isize != -1 {
                return Ok(Some(Conn(h)));
            }
            match win::last_error() {
                2 => return Ok(None),
                231 => unsafe {
                    WaitNamedPipeW(name.as_ptr(), 200);
                },
                e => return Err(io::Error::from_raw_os_error(e as i32)),
            }
        }
        Ok(None)
    }
}

#[cfg(unix)]
mod imp {
    use std::io::{self, Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    fn path(tag: &str) -> PathBuf {
        let name = if tag.is_empty() {
            "mux.sock".to_string()
        } else {
            format!("mux-{tag}.sock")
        };
        super::super::paths::state_dir().join(name)
    }

    /// Build tags of this user's mux sockets ("" = from before per-build names).
    pub(crate) fn endpoints() -> Vec<String> {
        let dir = super::super::paths::state_dir();
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                let stem = n.strip_suffix(".sock")?;
                if stem == "mux" {
                    Some(String::new())
                } else {
                    stem.strip_prefix("mux-").map(String::from)
                }
            })
            .collect()
    }

    pub(crate) struct Conn(UnixStream);

    impl Read for &Conn {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            (&self.0).read(buf)
        }
    }

    impl Write for &Conn {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            (&self.0).write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    pub(crate) struct Listener(UnixListener);

    impl Listener {
        pub(crate) fn accept(&self) -> io::Result<Conn> {
            self.0.accept().map(|(s, _)| Conn(s))
        }
    }

    pub(crate) fn bind(tag: &str) -> io::Result<Listener> {
        use std::os::unix::fs::PermissionsExt;
        let p = path(tag);
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        if UnixStream::connect(&p).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "mux already running",
            ));
        }
        let _ = std::fs::remove_file(&p);
        UnixListener::bind(&p).map(Listener)
    }

    pub(crate) fn connect(tag: &str) -> io::Result<Option<Conn>> {
        match UnixStream::connect(path(tag)) {
            Ok(s) => Ok(Some(Conn(s))),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
}
