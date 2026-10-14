//! Windows primitives shared by the transport and the conde CLI: process start
//! times, the current user's SID, owner-only security descriptors and DACLs, and
//! named pipes with deadlines (overlapped I/O).

use std::ffi::{OsStr, c_void};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::Path;
use std::ptr;
use std::time::Instant;

use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, ERROR_SEM_TIMEOUT, FILETIME, GENERIC_READ, GENERIC_WRITE, GetLastError,
    HANDLE, INVALID_HANDLE_VALUE, LocalFree, STILL_ACTIVE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, DACL_SECURITY_INFORMATION, GetAce, GetSecurityDescriptorDacl,
    GetTokenInformation, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_INBOUND, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT, WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetExitCodeProcess, GetProcessTimes, INFINITE, OpenProcess,
    OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION, WaitForMultipleObjects,
    WaitForSingleObject,
};

const PIPE_BUFFER: u32 = 65536;
/// Listening instances kept per pipe, so concurrent clients are not refused as busy.
const LISTENING: usize = 4;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

/// NUL-terminated UTF-16 for a Win32 W function.
pub fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

fn owned(h: HANDLE) -> io::Result<OwnedHandle> {
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a valid handle this process just received and owns from here on.
    Ok(unsafe { OwnedHandle::from_raw_handle(h as RawHandle) })
}

fn raw(h: &OwnedHandle) -> HANDLE {
    h.as_raw_handle() as HANDLE
}

fn millis_until(deadline: Option<Instant>) -> u32 {
    match deadline {
        None => INFINITE,
        Some(d) => {
            let left = d.saturating_duration_since(Instant::now());
            left.as_millis().min(u128::from(INFINITE - 1)) as u32
        }
    }
}

fn open_process(pid: u32) -> Option<OwnedHandle> {
    // SAFETY: plain call; a null result is handled.
    owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) }).ok()
}

/// Creation time of a live process as a FILETIME (100 ns ticks since 1601), the
/// value Claude Code publishes as `procStartFt`; None for a gone or unreadable one.
pub fn process_created(pid: u32) -> Option<u64> {
    let h = open_process(pid)?;
    let mut code = 0u32;
    // SAFETY: a process handle with query access; out-parameters are valid.
    if unsafe { GetExitCodeProcess(raw(&h), &mut code) } == 0 || code != STILL_ACTIVE as u32 {
        return None;
    }
    // SAFETY: FILETIME is plain integers.
    let mut t: [FILETIME; 4] = unsafe { std::mem::zeroed() };
    let [c, e, k, u] = &mut t;
    // SAFETY: as above.
    if unsafe { GetProcessTimes(raw(&h), c, e, k, u) } == 0 {
        return None;
    }
    Some((u64::from(t[0].dwHighDateTime) << 32) | u64::from(t[0].dwLowDateTime))
}

/// `n` random bytes from the system RNG, hex-encoded.
pub fn random_hex(n: usize) -> io::Result<String> {
    let mut buf = vec![0u8; n];
    // SAFETY: the buffer is valid for its length; the system-preferred RNG needs no handle.
    let status = unsafe {
        BCryptGenRandom(ptr::null_mut(), buf.as_mut_ptr(), n as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG)
    };
    if status != 0 {
        return Err(io::Error::other(format!("BCryptGenRandom failed: {status:#x}")));
    }
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn sid_string(sid: *mut c_void) -> io::Result<String> {
    let mut out: *mut u16 = ptr::null_mut();
    // SAFETY: `sid` points into a live SID; the result is LocalAlloc'd and freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut out) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a NUL-terminated wide string from the call above.
    let s = unsafe {
        let len = (0..).take_while(|&i| *out.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(out, len))
    };
    // SAFETY: allocated by ConvertSidToStringSidW.
    unsafe { LocalFree(out.cast()) };
    Ok(s)
}

fn token_user_sid(process: HANDLE) -> io::Result<String> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: a process handle with query access; out-parameter valid.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = owned(token)?;
    let mut len = 0u32;
    // SAFETY: size query with a null buffer.
    unsafe { GetTokenInformation(raw(&token), TokenUser, ptr::null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64 storage keeps TOKEN_USER aligned.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: the buffer holds `len` bytes.
    if unsafe { GetTokenInformation(raw(&token), TokenUser, buf.as_mut_ptr().cast(), len, &mut len) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call filled a TOKEN_USER at the start of the buffer.
    let user = unsafe { &*buf.as_ptr().cast::<TOKEN_USER>() };
    sid_string(user.User.Sid)
}

/// The SID (`S-1-5-21-...`) of the user this process runs as.
pub fn own_user_sid() -> io::Result<String> {
    // SAFETY: the pseudo handle needs no closing.
    token_user_sid(unsafe { GetCurrentProcess() })
}

/// The SID of the user process `pid` runs as.
pub fn process_user_sid(pid: u32) -> io::Result<String> {
    let h = open_process(pid).ok_or_else(io::Error::last_os_error)?;
    token_user_sid(raw(&h))
}

/// A security descriptor (and attributes pointing at it) parsed from SDDL.
pub struct Security {
    sd: PSECURITY_DESCRIPTOR,
    attrs: SECURITY_ATTRIBUTES,
}

// SAFETY: the descriptor is immutable once built and owned by this value.
unsafe impl Send for Security {}
unsafe impl Sync for Security {}

impl Security {
    pub fn from_sddl(sddl: &str) -> io::Result<Security> {
        let w = wide(OsStr::new(sddl));
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: valid NUL-terminated SDDL; the descriptor is LocalAlloc'd, freed in Drop.
        if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, ptr::null_mut()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        };
        Ok(Security { sd, attrs })
    }

    /// Only the current user: [`crate::owner_only_sddl`] for its SID.
    pub fn owner_only(container: bool) -> io::Result<Security> {
        Security::from_sddl(&crate::owner_only_sddl(&own_user_sid()?, container))
    }

    pub fn attributes(&self) -> *const SECURITY_ATTRIBUTES {
        &self.attrs
    }

    /// Replace `path`'s DACL with this descriptor's, protected from inheritance.
    pub fn apply(&self, path: &Path) -> io::Result<()> {
        let (mut present, mut defaulted) = (0, 0);
        let mut dacl: *mut ACL = ptr::null_mut();
        // SAFETY: a valid descriptor; out-parameters valid.
        if unsafe { GetSecurityDescriptorDacl(self.sd, &mut present, &mut dacl, &mut defaulted) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let w = wide(path.as_os_str());
        // SAFETY: NUL-terminated path, a DACL owned by our descriptor.
        let rc = unsafe {
            SetNamedSecurityInfoW(
                w.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                dacl,
                ptr::null(),
            )
        };
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
        Ok(())
    }
}

impl Drop for Security {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.sd) };
    }
}

/// SIDs other than `allowed` that an allow ACE of `path`'s DACL grants access to;
/// `["everyone (no DACL)"]` for a file without a DACL.
pub fn foreign_grants(path: &Path, allowed: &[&str]) -> io::Result<Vec<String>> {
    let w = wide(path.as_os_str());
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: NUL-terminated path; the descriptor is LocalAlloc'd and freed below.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            w.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut sd,
        )
    };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    let result = (|| {
        if dacl.is_null() {
            return Ok(vec!["everyone (no DACL)".to_string()]);
        }
        let mut out = Vec::new();
        // SAFETY: a DACL inside the descriptor returned above.
        let count = unsafe { (*dacl).AceCount };
        for i in 0..u32::from(count) {
            let mut ace: *mut c_void = ptr::null_mut();
            // SAFETY: index below AceCount.
            if unsafe { GetAce(dacl, i, &mut ace) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
            // SAFETY: every ACE starts with an ACE_HEADER; the SID of an allow ACE starts at SidStart.
            if unsafe { (*ace).Header.AceType } != ACCESS_ALLOWED_ACE_TYPE {
                continue;
            }
            let sid = sid_string(unsafe { (&raw mut (*ace).SidStart).cast() })?;
            if !allowed.contains(&sid.as_str()) && !out.contains(&sid) {
                out.push(sid);
            }
        }
        Ok(out)
    })();
    // SAFETY: allocated by GetNamedSecurityInfoW.
    unsafe { LocalFree(sd) };
    result
}

/// A manual-reset event for one overlapped operation.
struct Event(OwnedHandle);

impl Event {
    fn new() -> io::Result<Event> {
        // SAFETY: an unnamed manual-reset event.
        owned(unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) }).map(Event)
    }
}

/// One overlapped operation on `handle` waited for until `deadline`; past it the
/// operation is cancelled and reported as TimedOut.
fn overlapped(handle: HANDLE, deadline: Option<Instant>, start: impl FnOnce(*mut OVERLAPPED) -> i32) -> io::Result<u32> {
    let event = Event::new()?;
    // SAFETY: OVERLAPPED is plain data; zeroed is its documented initial state.
    let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
    ov.hEvent = raw(&event.0);
    if start(&mut ov) == 0 {
        // SAFETY: plain call.
        let err = unsafe { GetLastError() };
        if err != ERROR_IO_PENDING {
            return Err(io::Error::from_raw_os_error(err as i32));
        }
        // SAFETY: the event outlives the wait.
        if unsafe { WaitForSingleObject(raw(&event.0), millis_until(deadline)) } != WAIT_OBJECT_0 {
            // SAFETY: cancel our own operation, then wait for it so `ov` outlives it.
            unsafe { CancelIoEx(handle, &ov) };
            let mut n = 0u32;
            if unsafe { GetOverlappedResult(handle, &ov, &mut n, 1) } != 0 {
                return Ok(n);
            }
            let err = io::Error::last_os_error();
            return Err(if err.raw_os_error() == Some(ERROR_OPERATION_ABORTED as i32) {
                io::Error::new(io::ErrorKind::TimedOut, "pipe operation timed out")
            } else {
                err
            });
        }
    }
    let mut n = 0u32;
    // SAFETY: the operation completed; `ov` is the one it used.
    if unsafe { GetOverlappedResult(handle, &ov, &mut n, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(n)
}

fn read_handle(h: &OwnedHandle, buf: &mut [u8], deadline: Option<Instant>) -> io::Result<usize> {
    let len = buf.len().min(u32::MAX as usize) as u32;
    // SAFETY: `buf` stays borrowed until the operation completes inside `overlapped`.
    match overlapped(raw(h), deadline, |ov| unsafe { ReadFile(raw(h), buf.as_mut_ptr(), len, ptr::null_mut(), ov) }) {
        Ok(n) => Ok(n as usize),
        Err(e) if e.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) => Ok(0),
        Err(e) => Err(e),
    }
}

fn write_handle(h: &OwnedHandle, mut data: &[u8], deadline: Option<Instant>) -> io::Result<()> {
    while !data.is_empty() {
        let len = data.len().min(u32::MAX as usize) as u32;
        // SAFETY: `data` stays borrowed until the operation completes inside `overlapped`.
        let n = overlapped(raw(h), deadline, |ov| unsafe { WriteFile(raw(h), data.as_ptr(), len, ptr::null_mut(), ov) })?;
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        data = &data[n as usize..];
    }
    Ok(())
}

/// Whether a pipe of this name exists (also when every instance is busy).
pub fn pipe_exists(name: &OsStr) -> bool {
    let w = wide(name);
    // SAFETY: NUL-terminated name.
    if unsafe { WaitNamedPipeW(w.as_ptr(), 1) } != 0 {
        return true;
    }
    // SAFETY: plain call.
    unsafe { GetLastError() == ERROR_SEM_TIMEOUT }
}

/// The client end of a named pipe, opened for overlapped I/O with deadlines.
pub struct PipeClient {
    handle: OwnedHandle,
}

impl PipeClient {
    /// Open `name`, waiting while every instance is busy until `deadline`. The server
    /// may only identify, never impersonate, this client.
    pub fn connect(name: &OsStr, deadline: Option<Instant>) -> io::Result<PipeClient> {
        let w = wide(name);
        loop {
            // SAFETY: NUL-terminated name; no template or security attributes.
            let h = unsafe {
                CreateFileW(
                    w.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    ptr::null_mut(),
                )
            };
            if h != INVALID_HANDLE_VALUE {
                return Ok(PipeClient { handle: owned(h)? });
            }
            // SAFETY: plain call.
            let err = unsafe { GetLastError() };
            let late = deadline.is_some_and(|d| Instant::now() >= d);
            if err != ERROR_PIPE_BUSY || late {
                return Err(io::Error::from_raw_os_error(err as i32));
            }
            // SAFETY: NUL-terminated name.
            unsafe { WaitNamedPipeW(w.as_ptr(), millis_until(deadline).max(1)) };
        }
    }

    pub fn write_all(&self, data: &[u8], deadline: Option<Instant>) -> io::Result<()> {
        write_handle(&self.handle, data, deadline)
    }

    /// Bytes read, 0 at the end of the stream.
    pub fn read(&self, buf: &mut [u8], deadline: Option<Instant>) -> io::Result<usize> {
        read_handle(&self.handle, buf, deadline)
    }

    /// The pid of the process serving this pipe.
    pub fn server_pid(&self) -> io::Result<u32> {
        let mut pid = 0u32;
        // SAFETY: a pipe client handle; out-parameter valid.
        if unsafe { GetNamedPipeServerProcessId(raw(&self.handle), &mut pid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(pid)
    }
}

/// Blocking reads and writes without a deadline (the CLI's request/response).
impl io::Read for &PipeClient {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        PipeClient::read(self, buf, None)
    }
}

impl io::Write for &PipeClient {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        write_handle(&self.handle, buf, None).map(|()| buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One server instance waiting for a client.
struct Listening {
    /// Taken by `accept` once a client connected.
    handle: Option<OwnedHandle>,
    event: Event,
    ov: Box<OVERLAPPED>,
    connected: bool,
}

// SAFETY: the OVERLAPPED and its event belong to this instance alone; Windows
// handles are process-wide, so the owning thread may change.
unsafe impl Send for Listening {}

impl Drop for Listening {
    fn drop(&mut self) {
        if let Some(h) = self.handle.as_ref().filter(|_| !self.connected) {
            let mut n = 0u32;
            // SAFETY: cancel our pending connect and wait for it before `ov` is freed.
            unsafe {
                CancelIoEx(raw(h), &*self.ov);
                GetOverlappedResult(raw(h), &*self.ov, &mut n, 1);
            }
        }
    }
}

/// An inbound-only named pipe server with a few listening instances, accepting
/// with a deadline.
pub struct PipeListener {
    name: Vec<u16>,
    security: Security,
    waiting: Vec<Listening>,
}

impl PipeListener {
    /// Create `name` (it must not exist yet), local clients only, DACL from `security`.
    pub fn bind(name: &OsStr, security: Security) -> io::Result<PipeListener> {
        let mut l = PipeListener { name: wide(name), security, waiting: Vec::new() };
        let first = l.instance(true)?;
        l.waiting.push(first);
        l.refill()?;
        Ok(l)
    }

    fn instance(&self, first: bool) -> io::Result<Listening> {
        let flags = PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
        // SAFETY: NUL-terminated name, attributes owned by `self`.
        let h = unsafe {
            CreateNamedPipeW(
                self.name.as_ptr(),
                flags,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUFFER,
                PIPE_BUFFER,
                0,
                self.security.attributes(),
            )
        };
        let handle = owned(h)?;
        let event = Event::new()?;
        // SAFETY: zeroed OVERLAPPED is its initial state; boxed so its address is stable.
        let mut ov: Box<OVERLAPPED> = Box::new(unsafe { std::mem::zeroed() });
        ov.hEvent = raw(&event.0);
        let h = raw(&handle);
        let mut l = Listening { handle: Some(handle), event, ov, connected: false };
        // SAFETY: the OVERLAPPED lives in `l` until the connect completes or is cancelled.
        if unsafe { ConnectNamedPipe(h, &mut *l.ov) } == 0 {
            // SAFETY: plain call.
            match unsafe { GetLastError() } {
                ERROR_IO_PENDING => {}
                ERROR_PIPE_CONNECTED => l.connected = true,
                err => {
                    l.connected = true; // nothing pending to cancel
                    return Err(io::Error::from_raw_os_error(err as i32));
                }
            }
        } else {
            l.connected = true;
        }
        Ok(l)
    }

    fn refill(&mut self) -> io::Result<()> {
        while self.waiting.len() < LISTENING {
            let next = self.instance(false)?;
            self.waiting.push(next);
        }
        Ok(())
    }

    /// The next connected client, or None when none connected by `deadline`.
    pub fn accept(&mut self, deadline: Instant) -> io::Result<Option<PipeConn>> {
        loop {
            if let Some(i) = self.waiting.iter().position(|l| l.connected) {
                let handle = self.waiting.remove(i).handle.take();
                let _ = self.refill();
                if let Some(handle) = handle {
                    return Ok(Some(PipeConn { handle }));
                }
                continue;
            }
            if self.waiting.is_empty() {
                self.refill()?;
                continue;
            }
            let events: Vec<HANDLE> = self.waiting.iter().map(|l| raw(&l.event.0)).collect();
            // SAFETY: the events outlive the wait.
            let w = unsafe { WaitForMultipleObjects(events.len() as u32, events.as_ptr(), 0, millis_until(Some(deadline))) };
            if w == WAIT_TIMEOUT {
                return Ok(None);
            }
            let i = w.wrapping_sub(WAIT_OBJECT_0) as usize;
            if i >= self.waiting.len() {
                return Err(io::Error::last_os_error());
            }
            let l = &mut self.waiting[i];
            let mut n = 0u32;
            let h = l.handle.as_ref().map_or(ptr::null_mut(), raw);
            // SAFETY: the connect on this instance completed.
            let ok = unsafe { GetOverlappedResult(h, &*l.ov, &mut n, 0) } != 0;
            l.connected = true;
            if !ok {
                // a client that left before we looked: drop the instance, keep listening
                self.waiting.remove(i);
                self.refill()?;
            }
        }
    }
}

/// A connected client of a [`PipeListener`].
pub struct PipeConn {
    handle: OwnedHandle,
}

impl PipeConn {
    pub fn read(&self, buf: &mut [u8], deadline: Option<Instant>) -> io::Result<usize> {
        read_handle(&self.handle, buf, deadline)
    }
}

/// The pid of the client connected to the server end `pipe`.
pub fn client_pid(pipe: &impl AsRawHandle) -> io::Result<u32> {
    let mut pid = 0u32;
    // SAFETY: a live handle borrowed for the call; out-parameter valid.
    if unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle() as HANDLE, &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(pid)
}
