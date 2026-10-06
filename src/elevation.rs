//! Windows startup elevation through the standard UAC consent prompt.

#[derive(Debug, PartialEq, Eq)]
pub enum StartupDecision {
    Continue,
    Relaunched,
    Cancelled,
    Failed(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum StartupAction {
    RunInterface,
    ExitSuccess,
    ExitFailure,
}

pub fn startup_action(decision: &StartupDecision) -> StartupAction {
    match decision {
        StartupDecision::Continue => StartupAction::RunInterface,
        StartupDecision::Relaunched => StartupAction::ExitSuccess,
        StartupDecision::Cancelled | StartupDecision::Failed(_) => StartupAction::ExitFailure,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TokenStatus {
    Elevated,
    NeedsElevation,
}

fn token_status(is_elevated: bool) -> TokenStatus {
    if is_elevated {
        TokenStatus::Elevated
    } else {
        TokenStatus::NeedsElevation
    }
}

fn with_virtual_terminal_processing(original: u32) -> u32 {
    original | 0x0004
}

#[cfg(windows)]
pub struct ConsoleModeGuard {
    originals: Vec<(windows_sys::Win32::Foundation::HANDLE, u32)>,
}

#[cfg(windows)]
pub fn enable_virtual_terminal_processing() -> ConsoleModeGuard {
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        System::Console::{
            ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE,
            STD_OUTPUT_HANDLE, SetConsoleMode,
        },
    };

    let mut originals = Vec::with_capacity(2);
    for standard_handle in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: GetStdHandle accepts these documented standard-handle identifiers.
        let handle = unsafe { GetStdHandle(standard_handle) };
        if handle.is_null()
            || handle == INVALID_HANDLE_VALUE
            || originals.iter().any(|(seen, _)| *seen == handle)
        {
            continue;
        }

        let mut original = 0;
        // Redirected or non-console handles simply do not support console modes.
        // SAFETY: original is a valid writable output pointer.
        if unsafe { GetConsoleMode(handle, &mut original) } == 0 {
            continue;
        }
        if original & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0 {
            continue;
        }
        // SAFETY: preserve all existing bits while adding the documented VT flag.
        if unsafe { SetConsoleMode(handle, with_virtual_terminal_processing(original)) } != 0 {
            originals.push((handle, original));
        }
    }

    ConsoleModeGuard { originals }
}

#[cfg(windows)]
impl Drop for ConsoleModeGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::SetConsoleMode;

        for (handle, original) in self.originals.drain(..).rev() {
            // SAFETY: the standard handle remains owned by the process; restore only
            // modes this guard successfully changed, before the process exits.
            unsafe { SetConsoleMode(handle, original) };
        }
    }
}

#[cfg(windows)]
pub fn ensure_elevated() -> StartupDecision {
    match is_elevated() {
        Ok(is_elevated) if token_status(is_elevated) == TokenStatus::Elevated => {
            StartupDecision::Continue
        }
        Ok(_) => match relaunch_elevated() {
            Ok(()) => StartupDecision::Relaunched,
            Err(error) if error.raw_os_error() == Some(1223) => StartupDecision::Cancelled,
            Err(error) => StartupDecision::Failed(error.to_string()),
        },
        Err(error) => StartupDecision::Failed(error.to_string()),
    }
}

#[cfg(not(windows))]
pub fn ensure_elevated() -> StartupDecision {
    StartupDecision::Failed("esta operação requer o Windows".to_owned())
}

#[cfg(windows)]
fn is_elevated() -> std::io::Result<bool> {
    use std::{io, mem::size_of, ptr};
    use windows_sys::Win32::{
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    let mut token = ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a valid pseudo-handle and token is a writable output.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = TokenHandle(token);

    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut returned = 0;
    // SAFETY: elevation is writable for exactly its size, and token is owned by TokenHandle.
    let succeeded = unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    if returned < size_of::<TOKEN_ELEVATION>() as u32 {
        return Err(io::Error::other("informação de elevação incompleta"));
    }
    Ok(elevation.TokenIsElevated != 0)
}

#[cfg(windows)]
struct TokenHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for TokenHandle {
    fn drop(&mut self) {
        // SAFETY: this handle was returned by OpenProcessToken and is closed exactly once.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

#[cfg(windows)]
fn relaunch_elevated() -> std::io::Result<()> {
    use std::{ffi::OsStr, io, mem::size_of, os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::UI::{
        Shell::{SEE_MASK_DEFAULT, SHELLEXECUTEINFOW, ShellExecuteExW},
        WindowsAndMessaging::SW_SHOWNORMAL,
    };

    let executable = std::env::current_exe()?;
    let executable: Vec<u16> = executable
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let verb: Vec<u16> = OsStr::new("runas").encode_wide().chain(Some(0)).collect();
    let mut parameters = Vec::new();
    for (index, argument) in std::env::args_os().skip(1).enumerate() {
        if index != 0 {
            parameters.push(b' ' as u16);
        }
        let argument = argument.encode_wide().collect::<Vec<_>>();
        parameters.extend(quote_windows_argument(&argument));
    }
    parameters.push(0);

    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_DEFAULT,
        hwnd: ptr::null_mut(),
        lpVerb: verb.as_ptr(),
        lpFile: executable.as_ptr(),
        lpParameters: parameters.as_ptr(),
        lpDirectory: ptr::null(),
        nShow: SW_SHOWNORMAL,
        hInstApp: ptr::null_mut(),
        lpIDList: ptr::null_mut(),
        lpClass: ptr::null(),
        hkeyClass: ptr::null_mut(),
        dwHotKey: 0,
        Anonymous: unsafe { std::mem::zeroed() },
        hProcess: ptr::null_mut(),
    };

    // SAFETY: all pointers refer to live, NUL-terminated buffers and info is initialized.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn quote_windows_argument(value: &[u16]) -> Vec<u16> {
    let mut quoted = vec![b'"' as u16];
    let mut slashes = 0;
    for unit in value.iter().copied() {
        if unit == b'\\' as u16 {
            slashes += 1;
        } else if unit == b'"' as u16 {
            quoted.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2 + 1));
            quoted.push(b'"' as u16);
            slashes = 0;
        } else {
            quoted.extend(std::iter::repeat_n(b'\\' as u16, slashes));
            quoted.push(unit);
            slashes = 0;
        }
    }
    quoted.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    quoted.push(b'"' as u16);
    quoted
}

#[cfg(test)]
mod tests {
    use super::{
        StartupAction, StartupDecision, TokenStatus, quote_windows_argument, startup_action,
        token_status, with_virtual_terminal_processing,
    };

    #[test]
    fn elevated_token_skips_relaunch_and_limited_token_requires_it() {
        assert_eq!(token_status(true), TokenStatus::Elevated);
        assert_eq!(token_status(false), TokenStatus::NeedsElevation);
    }

    #[test]
    fn virtual_terminal_mode_preserves_existing_flags_and_adds_only_vt_processing() {
        assert_eq!(with_virtual_terminal_processing(0), 0x0004);
        assert_eq!(with_virtual_terminal_processing(0x0001 | 0x0008), 0x000d);
    }

    #[test]
    fn elevated_relaunch_argument_quoting_preserves_quotes_unicode_and_trailing_slashes() {
        assert_eq!(
            quote_windows_argument(&"a b".encode_utf16().collect::<Vec<_>>()),
            "\"a b\"".encode_utf16().collect::<Vec<_>>()
        );
        assert_eq!(
            quote_windows_argument(&[b'a' as u16, b'\\' as u16]),
            "\"a\\\\\"".encode_utf16().collect::<Vec<_>>()
        );
        assert_eq!(
            quote_windows_argument(&[b'a' as u16, b'"' as u16, b'b' as u16]),
            "\"a\\\"b\"".encode_utf16().collect::<Vec<_>>()
        );
        let unicode = "💽".encode_utf16().collect::<Vec<_>>();
        assert_eq!(quote_windows_argument(&unicode)[1..3], unicode);
    }

    #[test]
    fn startup_exit_actions_fail_on_cancel_or_elevation_error() {
        assert_eq!(
            startup_action(&StartupDecision::Continue),
            StartupAction::RunInterface
        );
        assert_eq!(
            startup_action(&StartupDecision::Relaunched),
            StartupAction::ExitSuccess
        );
        assert_eq!(
            startup_action(&StartupDecision::Cancelled),
            StartupAction::ExitFailure
        );
        assert_eq!(
            startup_action(&StartupDecision::Failed("failure".to_owned())),
            StartupAction::ExitFailure
        );
    }
}
