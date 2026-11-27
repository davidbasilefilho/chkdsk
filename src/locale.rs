//! Resolve the first supported Windows user display language.
pub fn tr(pt: &'static str, en: &'static str) -> &'static str {
    if english() { en } else { pt }
}
#[cfg(any(windows, test))]
fn prefer_english<'a>(languages: impl Iterator<Item = &'a str>) -> bool {
    for language in languages {
        match language
            .split('-')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "en" => return true,
            "pt" => return false,
            _ => (),
        }
    }
    true
}

#[cfg(all(windows, not(test)))]
pub fn english() -> bool {
    use std::sync::OnceLock;
    use windows_sys::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
    static LANGUAGE: OnceLock<bool> = OnceLock::new();
    *LANGUAGE.get_or_init(|| {
        let (mut count, mut len) = (0, 0);
        // SAFETY: first query obtains the required MULTI_SZ capacity.
        if unsafe {
            GetUserPreferredUILanguages(
                MUI_LANGUAGE_NAME,
                &mut count,
                std::ptr::null_mut(),
                &mut len,
            )
        } == 0
            || len > 65536
        {
            return true;
        }
        let mut data = vec![0; len as usize];
        // SAFETY: buffer contains len UTF-16 code units.
        if unsafe {
            GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, data.as_mut_ptr(), &mut len)
        } == 0
        {
            return true;
        }
        let names = String::from_utf16_lossy(&data);
        prefer_english(names.split('\0'))
    })
}
#[cfg(all(not(windows), not(test)))]
pub fn english() -> bool {
    true
}
#[cfg(test)]
thread_local! { pub(crate) static TEST_ENGLISH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[cfg(test)]
pub fn english() -> bool {
    TEST_ENGLISH.get()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordered_language_preferences() {
        assert!(!prefer_english(["fr-FR", "pt-BR", "en-US"].into_iter()));
        assert!(prefer_english(["en-US", "pt-BR"].into_iter()));
        assert!(prefer_english(["de-DE"].into_iter()));
    }
}
