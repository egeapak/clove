//! Windows identity helpers: the SID of the current user and of the process at
//! the other end of the hub pipe. The pipe name alone proves nothing — any user
//! can create a pipe of any unclaimed name — so both ends compare the peer's
//! token user against their own before trusting a connection.

use std::io;

use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// The current user's SID, as a string (`S-1-5-21-…`).
pub fn current_user_sid() -> io::Result<String> {
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing.
    let process = unsafe { GetCurrentProcess() };
    token_user_sid(process)
}

/// The SID of the user a process runs as.
pub fn process_user_sid(pid: u32) -> io::Result<String> {
    // SAFETY: a plain OpenProcess; the handle is closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let sid = token_user_sid(process);
    // SAFETY: `process` is a handle we own.
    unsafe { CloseHandle(process) };
    sid
}

fn token_user_sid(process: HANDLE) -> io::Result<String> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `token` receives a handle we close below.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let sid = token_sid(token);
    // SAFETY: `token` is a handle we own.
    unsafe { CloseHandle(token) };
    sid
}

fn token_sid(token: HANDLE) -> io::Result<String> {
    let mut len = 0u32;
    // SAFETY: a size query (null buffer, zero length); it fails with the size.
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64 storage keeps TOKEN_USER suitably aligned.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` holds at least `len` bytes.
    if unsafe { GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetTokenInformation(TokenUser) filled `buf` with a TOKEN_USER.
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: the SID points into `buf`, alive for this call; `text` is
    // LocalAlloc'd by the API and freed below.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `text` is a NUL-terminated UTF-16 string from the API.
    let sid = unsafe {
        let mut n = 0;
        while *text.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(text, n))
    };
    // SAFETY: `text` was allocated by ConvertSidToStringSidW with LocalAlloc.
    unsafe { LocalFree(text.cast()) };
    Ok(sid)
}
