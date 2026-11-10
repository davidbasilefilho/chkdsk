//! Startup elevation through the standard UAC consent prompt.
//!
//! The app is a GUI process with no console, so nothing here prints: the caller
//! reports failures in a message box. A limited process relaunches itself once;
//! the relaunched copy carries [`RELAUNCH_FLAG`] and never relaunches again, so
//! a refused or ineffective elevation cannot loop.

/// Appended to the arguments of the elevated copy. Unknown to rune, which passes it through.
pub(crate) const RELAUNCH_FLAG: &str = "--elevated-relaunch";

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
enum Plan {
    Run,
    Relaunch,
}

/// Relaunch only a limited token that has not been relaunched already.
fn plan(is_elevated: bool, relaunched: bool) -> Plan {
    if is_elevated || relaunched {
        Plan::Run
    } else {
        Plan::Relaunch
    }
}

fn is_relaunched<I>(arguments: I) -> bool
where
    I: IntoIterator,
    I::Item: AsRef<std::ffi::OsStr>,
{
    arguments
        .into_iter()
        .any(|argument| argument.as_ref() == RELAUNCH_FLAG)
}

fn com_initialization_succeeded(result: i32) -> bool {
    result >= 0
}

/// Elevate when the access to the disks needs it. Elsewhere there is nothing to elevate.
#[cfg(windows)]
pub fn ensure_elevated() -> StartupDecision {
    match is_elevated() {
        Ok(elevated) => match plan(elevated, is_relaunched(std::env::args_os().skip(1))) {
            Plan::Run => StartupDecision::Continue,
            Plan::Relaunch => match relaunch_elevated() {
                Ok(()) => StartupDecision::Relaunched,
                Err(error) if error.raw_os_error() == Some(1223) => StartupDecision::Cancelled,
                Err(error) => StartupDecision::Failed(error.to_string()),
            },
        },
        Err(error) => StartupDecision::Failed(error.to_string()),
    }
}

#[cfg(not(windows))]
pub fn ensure_elevated() -> StartupDecision {
    StartupDecision::Continue
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
        Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW},
        WindowsAndMessaging::SW_SHOWNORMAL,
    };

    let executable = std::env::current_exe()?;
    let executable: Vec<u16> = executable
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let verb: Vec<u16> = OsStr::new("runas").encode_wide().chain(Some(0)).collect();
    // The relaunch marker stops a process that is still limited from asking again.
    let arguments = std::env::args_os()
        .skip(1)
        .chain(Some(OsStr::new(RELAUNCH_FLAG).to_owned()));
    let mut parameters = Vec::new();
    for (index, argument) in arguments.enumerate() {
        if index != 0 {
            parameters.push(b' ' as u16);
        }
        let argument = argument.encode_wide().collect::<Vec<_>>();
        parameters.extend(quote_windows_argument(&argument));
    }
    parameters.push(0);

    let _com_apartment = ComApartmentGuard::initialize()?;

    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC,
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
struct ComApartmentGuard {
    uninitialize_on_drop: bool,
}

#[cfg(windows)]
impl ComApartmentGuard {
    fn initialize() -> std::io::Result<Self> {
        use std::ptr;
        use windows_sys::Win32::{
            Foundation::RPC_E_CHANGED_MODE,
            System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx},
        };

        // SAFETY: null is the documented reserved parameter; this thread owns the
        // resulting COM initialization count until this guard is dropped.
        let result = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        if com_initialization_succeeded(result) {
            Ok(Self {
                uninitialize_on_drop: true,
            })
        } else if result == RPC_E_CHANGED_MODE {
            // COM is already initialized on this thread with another apartment model.
            Ok(Self {
                uninitialize_on_drop: false,
            })
        } else {
            Err(std::io::Error::from_raw_os_error(result))
        }
    }
}

#[cfg(windows)]
impl Drop for ComApartmentGuard {
    fn drop(&mut self) {
        if self.uninitialize_on_drop {
            // SAFETY: initialize succeeded (S_OK or S_FALSE) and this balances our
            // one CoInitializeEx call without affecting any pre-existing COM user.
            unsafe { windows_sys::Win32::System::Com::CoUninitialize() };
        }
    }
}

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
    #[cfg(not(windows))]
    use super::ensure_elevated;
    use super::{
        Plan, RELAUNCH_FLAG, StartupAction, StartupDecision, com_initialization_succeeded,
        is_relaunched, plan, quote_windows_argument, startup_action,
    };

    #[test]
    fn only_a_limited_first_launch_relaunches() {
        assert_eq!(plan(true, false), Plan::Run);
        assert_eq!(plan(false, false), Plan::Relaunch);
        // The relaunched copy runs even when it is still limited: no loop.
        assert_eq!(plan(false, true), Plan::Run);
        assert_eq!(plan(true, true), Plan::Run);
    }

    #[test]
    fn the_relaunch_marker_is_recognized_among_other_arguments() {
        assert!(!is_relaunched(["--snap", "a.png"]));
        assert!(!is_relaunched(Vec::<&str>::new()));
        assert!(is_relaunched(["--snap", RELAUNCH_FLAG]));
        assert!(!is_relaunched(["--elevated-relaunch=1"]));
    }

    #[cfg(not(windows))]
    #[test]
    fn elsewhere_elevation_is_a_no_op() {
        assert_eq!(ensure_elevated(), StartupDecision::Continue);
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

    #[test]
    fn com_uninitialization_is_owned_only_after_successful_initialization() {
        assert!(com_initialization_succeeded(0)); // S_OK
        assert!(com_initialization_succeeded(1)); // S_FALSE
        assert!(!com_initialization_succeeded(-2)); // Includes RPC_E_CHANGED_MODE.
    }
}
