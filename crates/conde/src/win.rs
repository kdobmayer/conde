//! Windows-only OS calls of the conde binary; the shared ones (SIDs, DACLs, pipes,
//! process start times) are in `conde_transport::win`.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, RawHandle};
use std::path::Path;
use std::ptr;

use conde_transport::win::{Security, wide};
use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation};
use windows_sys::Win32::Foundation::{GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, UNICODE_STRING};
use windows_sys::Win32::Storage::FileSystem::{CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL};
use windows_sys::Win32::System::Console::{
    ENABLE_ECHO_INPUT, FreeConsole, GetConsoleMode, GetConsoleProcessList, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    SetConsoleMode, SetStdHandle,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::SystemInformation::{ComputerNameDnsHostname, GetComputerNameExW};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

fn owned(h: HANDLE) -> io::Result<std::os::windows::io::OwnedHandle> {
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a fresh handle this process owns from here on.
    Ok(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(h as RawHandle) })
}

/// The machine's DNS host name.
pub fn hostname() -> Option<String> {
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    // SAFETY: the buffer holds `len` UTF-16 units.
    if unsafe { GetComputerNameExW(ComputerNameDnsHostname, buf.as_mut_ptr(), &mut len) } == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

/// Create `dir` (the leaf; its parents must exist) readable by the current user only.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    let security = Security::owner_only(true)?;
    let w = wide(dir.as_os_str());
    // SAFETY: NUL-terminated path, attributes alive for the call.
    if unsafe { CreateDirectoryW(w.as_ptr(), security.attributes()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Create a new file readable by the current user only, from its first byte.
pub fn create_private_file(path: &Path) -> io::Result<File> {
    let security = Security::owner_only(false)?;
    let w = wide(path.as_os_str());
    // SAFETY: NUL-terminated path, attributes alive for the call.
    let h = unsafe {
        CreateFileW(w.as_ptr(), GENERIC_WRITE, 0, security.attributes(), CREATE_NEW, FILE_ATTRIBUTE_NORMAL, ptr::null_mut())
    };
    owned(h).map(File::from)
}

/// Read one line from the console with echo off, after `prompt`.
pub fn read_secret(prompt: &str) -> io::Result<String> {
    let input = std::fs::OpenOptions::new().read(true).write(true).open("CONIN$")?;
    let mut output = std::fs::OpenOptions::new().write(true).open("CONOUT$")?;
    let h = input.as_raw_handle() as HANDLE;
    let mut saved = 0u32;
    // SAFETY: a console input handle; out-parameter valid.
    if unsafe { GetConsoleMode(h, &mut saved) } == 0 {
        return Err(io::Error::last_os_error());
    }
    output.write_all(prompt.as_bytes())?;
    // SAFETY: as above.
    unsafe { SetConsoleMode(h, saved & !ENABLE_ECHO_INPUT) };
    let mut line = String::new();
    let read = io::BufReader::new(&input).read_line(&mut line);
    // SAFETY: as above.
    unsafe { SetConsoleMode(h, saved) };
    let _ = output.write_all(b"\r\n");
    read?;
    Ok(line.trim().to_string())
}

/// When this process alone owns its console (Task Scheduler made it for the bridge),
/// send stdout and stderr to `log` and give the console up, which closes its window.
pub fn detach_console(log: &Path) -> io::Result<bool> {
    let mut pids = [0u32; 4];
    // SAFETY: the buffer holds 4 pids.
    let attached = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) };
    if attached != 1 {
        return Ok(false);
    }
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
    let h = file.into_raw_handle() as HANDLE;
    // SAFETY: the log handle stays open for the process lifetime (never closed).
    unsafe {
        SetStdHandle(STD_OUTPUT_HANDLE, h);
        SetStdHandle(STD_ERROR_HANDLE, h);
        FreeConsole();
    }
    Ok(true)
}

/// Every process: pid -> (parent pid, executable file name), one Toolhelp snapshot.
pub fn process_snapshot() -> io::Result<HashMap<u32, (u32, String)>> {
    // SAFETY: plain call; the handle is owned below.
    let snap = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) })?;
    let h = snap.as_raw_handle() as HANDLE;
    // SAFETY: PROCESSENTRY32W is plain data; dwSize must be set before the first call.
    let mut e: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut out = HashMap::new();
    // SAFETY: a snapshot handle and an initialized entry.
    let mut more = unsafe { Process32FirstW(h, &mut e) } != 0;
    while more {
        let len = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
        out.insert(e.th32ProcessID, (e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..len])));
        // SAFETY: as above.
        more = unsafe { Process32NextW(h, &mut e) } != 0;
    }
    Ok(out)
}

/// The command line of `pid` (Windows 8.1+), None when unreadable.
pub fn command_line(pid: u32) -> Option<String> {
    // SAFETY: plain call; the handle is owned below.
    let process = owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) }).ok()?;
    let h = process.as_raw_handle() as HANDLE;
    let mut len = 0u32;
    // SAFETY: size query with an empty buffer.
    unsafe { NtQueryInformationProcess(h, ProcessCommandLineInformation, ptr::null_mut(), 0, &mut len) };
    if (len as usize) < std::mem::size_of::<UNICODE_STRING>() {
        return None;
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: the buffer holds `len` bytes, u64-aligned for the UNICODE_STRING header.
    if unsafe { NtQueryInformationProcess(h, ProcessCommandLineInformation, buf.as_mut_ptr().cast(), len, &mut len) } != 0 {
        return None;
    }
    // SAFETY: the call wrote a UNICODE_STRING whose buffer points into `buf`.
    let s = unsafe { &*buf.as_ptr().cast::<UNICODE_STRING>() };
    if s.Buffer.is_null() {
        return None;
    }
    // SAFETY: `Length` bytes of UTF-16 inside `buf`.
    let units = unsafe { std::slice::from_raw_parts(s.Buffer, usize::from(s.Length) / 2) };
    Some(String::from_utf16_lossy(units))
}
