#![cfg_attr(not(windows), allow(dead_code))]

pub mod collector;
#[cfg(any(windows, test))]
pub mod consent;
pub mod export;
#[cfg(any(test, feature = "scenes"))]
pub mod fixtures;
pub mod locale;
pub mod present;
pub mod settings;
pub mod ui;

/// Embedded in each executable and checked when packaging or publishing.
/// The profile follows rustc's effective ISA, including dependency codegen.
pub const CPU_PROFILE: &str = if cfg!(target_arch = "aarch64") {
    "disksonar-cpu:arm64;"
} else if cfg!(all(
    target_feature = "avx2",
    target_feature = "bmi2",
    target_feature = "fma"
)) {
    "disksonar-cpu:x64;"
} else {
    "disksonar-cpu:x64-baseline;"
};

pub use disksonar_core::{health, history, model, parse};
pub use disksonar_windows::backend;
