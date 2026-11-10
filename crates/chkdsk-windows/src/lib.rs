//! Windows-only disk access: SMART/NVMe telemetry and UAC elevation. Other
//! platforms get stubs, so the workspace builds and tests everywhere.
#![cfg_attr(not(windows), allow(dead_code))]

pub mod backend;
pub mod elevation;
