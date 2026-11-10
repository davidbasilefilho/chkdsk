//! The Windows Save dialog (`GetSaveFileNameW`), offering JSON and CSV.

use super::{Format, Pick, with_extension};
use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::Path, path::PathBuf};
use windows_sys::Win32::UI::{
    Controls::Dialogs::{
        GetSaveFileNameW, OFN_DONTADDTORECENT, OFN_EXPLORER, OFN_HIDEREADONLY, OFN_NOCHANGEDIR,
        OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    },
    WindowsAndMessaging::{IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_YESNO, MessageBoxW},
};

/// Room for a long path: 32 K UTF-16 units, the Windows limit.
const PATH_UNITS: usize = 32 * 1024;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Filter pairs of a label and a pattern, each NUL-terminated, the list ended by an extra NUL.
fn filters() -> Vec<u16> {
    let mut list = Vec::new();
    for part in ["JSON (*.json)", "*.json", "CSV (*.csv)", "*.csv"] {
        list.extend(part.encode_utf16());
        list.push(0);
    }
    list.push(0);
    list
}

/// Blocks until the user saves or cancels, so call it off the window's thread.
pub fn pick(owner: usize, suggested_name: &str) -> Option<Pick> {
    let mut file = vec![0u16; PATH_UNITS];
    for (slot, unit) in file.iter_mut().zip(suggested_name.encode_utf16()) {
        *slot = unit;
    }
    let filters = filters();
    let title = wide("Exportar discos");
    // SAFETY: `OPENFILENAMEW` is plain data, and all-zero is its documented empty state.
    let mut dialog: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    dialog.hwndOwner = owner as _;
    dialog.lpstrFilter = filters.as_ptr();
    dialog.nFilterIndex = 1;
    dialog.lpstrFile = file.as_mut_ptr();
    dialog.nMaxFile = PATH_UNITS as u32;
    dialog.lpstrTitle = title.as_ptr();
    // No default extension: the chosen filter adds its own (`with_extension`), where a
    // fixed `.json` would name a CSV choice wrongly.
    dialog.Flags = OFN_EXPLORER
        | OFN_OVERWRITEPROMPT
        | OFN_PATHMUSTEXIST
        | OFN_NOCHANGEDIR
        | OFN_HIDEREADONLY
        | OFN_DONTADDTORECENT;
    // SAFETY: every pointer in `dialog` refers to a buffer that outlives the call, and
    // `lpstrFile` has room for `nMaxFile` units. A false return is a cancel or an error.
    if unsafe { GetSaveFileNameW(&mut dialog) } == 0 {
        return None;
    }
    let length = file.iter().position(|unit| *unit == 0)?;
    let typed = PathBuf::from(OsString::from_wide(&file[..length]));
    let filter = if dialog.nFilterIndex == 2 {
        Format::Csv
    } else {
        Format::Json
    };
    let path = with_extension(typed.clone(), filter);
    // The dialog checked the name as typed; one that gained an extension needs its own check.
    if path != typed && path.exists() && !confirm_replace(owner, &path) {
        return None;
    }
    Some(Pick { path, filter })
}

/// Ask whether to replace a file the dialog could not warn about.
fn confirm_replace(owner: usize, path: &Path) -> bool {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let message = wide(&format!("{name} já existe. Deseja substituí-lo?"));
    let title = wide("Exportar discos");
    // SAFETY: both buffers are NUL-terminated and outlive the call; `owner` is a window
    // handle or zero.
    let answer = unsafe {
        MessageBoxW(
            owner as _,
            message.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        )
    };
    answer == IDYES
}
