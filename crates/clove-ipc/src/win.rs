//! Windows identity helpers: the SID of the current user, of the hub pipe's
//! owner, and of the client at the other end of the pipe. The pipe name alone
//! proves nothing — any user can create a pipe of any unclaimed name — so both
//! ends check the other before trusting a connection. Both checks read the
//! connected handle itself, never a process id, which could be reused by an
//! unrelated process between the lookup and the check.

use std::io;
use std::os::windows::io::{AsRawHandle as _, BorrowedHandle};

use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetSecurityInfo, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, RevertToSelf, TokenUser, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    PSID, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken,
};

/// The current user's SID, as a string (`S-1-5-21-…`).
pub fn current_user_sid() -> io::Result<String> {
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing.
    let process = unsafe { GetCurrentProcess() };
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

/// The owner of the kernel object behind `handle` — for the client, the hub's
/// pipe, which the hub creates with its user as owner.
pub fn object_owner_sid(handle: BorrowedHandle<'_>) -> io::Result<String> {
    let mut owner: PSID = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `owner` points into `descriptor`, which the API allocates with
    // LocalAlloc and we free below; the other outputs are not requested.
    let status = unsafe {
        GetSecurityInfo(
            handle.as_raw_handle(),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let sid = sid_string(owner);
    // SAFETY: `descriptor` was allocated by GetSecurityInfo with LocalAlloc.
    unsafe { LocalFree(descriptor) };
    sid
}

/// The user of the client connected to the server end of the pipe `handle`,
/// read by briefly impersonating it on this thread. The client must have sent
/// something first (Windows impersonates only after a read).
pub fn pipe_client_sid(handle: BorrowedHandle<'_>) -> io::Result<String> {
    // SAFETY: impersonation applies to this thread only and is reverted below
    // before anything else runs on it.
    if unsafe { ImpersonateNamedPipeClient(handle.as_raw_handle()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: opened as self (the hub's access check), closed below.
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) };
    let error = io::Error::last_os_error();
    // SAFETY: undo the impersonation started above.
    let reverted = unsafe { RevertToSelf() };
    if reverted == 0 {
        // Still impersonating: nothing on this thread may run as the client.
        std::process::abort();
    }
    if opened == 0 {
        return Err(error);
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
    sid_string(user.User.Sid)
}

fn sid_string(sid: PSID) -> io::Result<String> {
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: `sid` is valid for this call; `text` is LocalAlloc'd by the API
    // and freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
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
