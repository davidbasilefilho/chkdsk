#![cfg_attr(not(windows), allow(dead_code))]

pub mod backend;
pub mod collector;
#[cfg(any(test, feature = "scenes"))]
pub mod fixtures;
pub mod health;
pub mod history;
pub mod model;
pub mod parse;
pub mod present;
pub mod ui;

/// Embedded in each executable and checked when packaging or publishing.
/// The profile follows rustc's effective ISA, including dependency codegen.
pub const CPU_PROFILE: &str = if cfg!(target_arch = "aarch64") {
    "chkdsk-cpu:arm64;"
} else if cfg!(all(
    target_feature = "avx2",
    target_feature = "bmi2",
    target_feature = "fma"
)) {
    "chkdsk-cpu:x64;"
} else {
    "chkdsk-cpu:x64-baseline;"
};
