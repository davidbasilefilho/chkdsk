//! User interface preferences stored beside, but independently of, diagnostic history.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Motion {
    #[default]
    System,
    On,
    Off,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    pub motion: Motion,
}
impl Settings {
    pub fn dark(self, system: bool) -> bool {
        match self.theme {
            Theme::System => system,
            Theme::Light => false,
            Theme::Dark => true,
        }
    }
    pub fn reduced(self, system: bool) -> bool {
        match self.motion {
            Motion::System => system,
            Motion::On => false,
            Motion::Off => true,
        }
    }
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|v| serde_json::from_slice(&v).ok())
            .unwrap_or_default()
    }
    pub fn save(self, path: &Path) -> std::io::Result<()> {
        crate::collector::write_snapshot(path, &self)
    }
}

pub fn user_file() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(|root| PathBuf::from(root).join("disksonar").join("settings.json"))
}

/// Rune refreshes the native preference on focus. Also observe it while our window
/// retains focus (for example when another process changes the user preference).
#[cfg(windows)]
pub fn observe_motion(mut changed: impl FnMut(bool) + Send + 'static) {
    std::thread::spawn(move || {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
        };
        let mut previous = None;
        loop {
            let mut enabled = 1i32;
            // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL into this local.
            let ok = unsafe {
                SystemParametersInfoW(
                    SPI_GETCLIENTAREAANIMATION,
                    0,
                    (&mut enabled as *mut i32).cast(),
                    0,
                )
            };
            if ok != 0 && previous != Some(enabled == 0) {
                previous = Some(enabled == 0);
                changed(enabled == 0);
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_reopen_and_recover_from_invalid_input() {
        let path = std::env::temp_dir().join(format!(
            "disksonar-settings-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let initial = Settings::load(&path);
        for value in [false, true] {
            assert_eq!(initial.dark(value), value);
            assert_eq!(initial.reduced(value), value);
        }
        let choice = Settings {
            theme: Theme::Dark,
            motion: Motion::Off,
        };
        choice.save(&path).unwrap();
        let reopened = Settings::load(&path);
        assert_eq!(choice, reopened);
        assert!(reopened.dark(false) && reopened.reduced(false));
        std::fs::write(&path, br#"{"theme":"unknown"}"#).unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        std::fs::remove_file(path).unwrap();
    }
}
