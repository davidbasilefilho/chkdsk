//! A normal Windows consent prompt. The original window stays responsive until
//! the process returned by ShellExecuteEx owns a visible window of its own.
use std::{
    mem::size_of,
    os::windows::ffi::OsStrExt,
    ptr::null_mut,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, HWND, LPARAM},
    Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
    System::{
        Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize},
        Threading::{GetCurrentProcess, GetProcessId, OpenProcessToken, WaitForSingleObject},
    },
    UI::{
        Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
        WindowsAndMessaging::{
            EnumWindows, GetWindowThreadProcessId, IsWindowVisible, SW_SHOWNORMAL,
        },
    },
};

struct Owned(HANDLE);
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn elevated() -> Option<bool> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let token = Owned(token);
    let mut info = TOKEN_ELEVATION::default();
    let mut length = 0;
    let success = unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            (&mut info as *mut TOKEN_ELEVATION).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut length,
        )
    };
    (success != 0).then_some(info.TokenIsElevated != 0)
}

struct WindowSearch {
    pid: u32,
    found: bool,
}
unsafe extern "system" fn find_window(hwnd: HWND, data: LPARAM) -> i32 {
    // SAFETY: EnumWindows is synchronous and data points to our live search.
    let search = unsafe { &mut *(data as *mut WindowSearch) };
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid);
    }
    if pid == search.pid && unsafe { IsWindowVisible(hwnd) } != 0 {
        search.found = true;
        return 0;
    }
    1
}

pub fn relaunch(selection: u64, theme: Option<&str>) -> bool {
    if elevated() != Some(false) {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let file: Vec<_> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let verb: Vec<_> = "runas".encode_utf16().chain(Some(0)).collect();
    let mut args = format!("--resume-drive={selection:016x}");
    if let Some(theme @ ("dark" | "light" | "system")) = theme {
        args.push_str(" --theme ");
        args.push_str(theme);
    }
    let params: Vec<_> = args.encode_utf16().chain(Some(0)).collect();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: params.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    let init = unsafe {
        CoInitializeEx(
            null_mut(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    if init < 0 {
        return false;
    }
    let launched = unsafe { ShellExecuteExW(&mut info) };
    unsafe {
        CoUninitialize();
    }
    if launched == 0 || info.hProcess.is_null() {
        return false;
    }
    let process = Owned(info.hProcess);
    let mut search = WindowSearch {
        pid: unsafe { GetProcessId(process.0) },
        found: false,
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && unsafe { WaitForSingleObject(process.0, 100) } == 258 {
        unsafe {
            EnumWindows(
                Some(find_window),
                (&mut search as *mut WindowSearch) as LPARAM,
            );
        }
        if search.found {
            return true;
        }
    }
    false
}
