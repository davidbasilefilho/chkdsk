//! Platform-independent disk model: telemetry types, raw SMART/NVMe parsing,
//! ATA profiles, health scoring and the on-disk history.

pub mod ata;
pub mod health;
pub mod history;
pub mod model;
pub mod parse;
