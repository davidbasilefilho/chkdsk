#![cfg_attr(not(windows), allow(dead_code))]

pub mod backend;
pub mod collector;
#[cfg(any(windows, test))]
pub mod consent;
#[cfg(any(test, feature = "scenes"))]
pub mod fixtures;
pub mod health;
pub mod history;
pub mod locale;
pub mod model;
pub mod parse;
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
