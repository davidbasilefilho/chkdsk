//! The Windows Save dialog (`GetSaveFileNameW`), offering JSON and CSV.

use super::{Format, Pick};
use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};
use windows_sys::Win32::UI::Controls::Dialogs::{
    GetSaveFileNameW, OFN_EXPLORER, OFN_HIDEREADONLY, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT,
    OFN_PATHMUSTEXIST, OPENFILENAMEW,
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
    let default_extension = wide(Format::Json.extension());
    // SAFETY: `OPENFILENAMEW` is plain data, and all-zero is its documented empty state.
    let mut dialog: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    dialog.hwndOwner = owner as _;
    dialog.lpstrFilter = filters.as_ptr();
    dialog.nFilterIndex = 1;
    dialog.lpstrFile = file.as_mut_ptr();
    dialog.nMaxFile = PATH_UNITS as u32;
    dialog.lpstrTitle = title.as_ptr();
    dialog.lpstrDefExt = default_extension.as_ptr();
    dialog.Flags =
        OFN_EXPLORER | OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | OFN_HIDEREADONLY;
    // SAFETY: every pointer in `dialog` refers to a buffer that outlives the call, and
    // `lpstrFile` has room for `nMaxFile` units. A false return is a cancel or an error.
    if unsafe { GetSaveFileNameW(&mut dialog) } == 0 {
        return None;
    }
    let length = file.iter().position(|unit| *unit == 0)?;
    Some(Pick {
        path: PathBuf::from(OsString::from_wide(&file[..length])),
        filter: if dialog.nFilterIndex == 2 {
            Format::Csv
        } else {
            Format::Json
        },
    })
}
