//! Shared, transport-independent domain types for disk health.

use serde::{Deserialize, Serialize};

/// A normalized disk identity. `id` should remain stable across enumeration;
/// `index` is the current OS enumeration index and may change between boots.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskIdentity {
    pub id: String,
    pub index: u32,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub firmware: Option<String>,
    pub capacity_bytes: Option<u64>,
    /// Normalized transport label, e.g. "NVMe", "SATA", or "USB".
    pub bus: Option<String>,
    pub kind: DiskKind,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskKind {
    Hdd,
    Ssd,
    Nvme,
    #[default]
    Unknown,
}

/// Query outcome is explicit: an absent value is not confused with a failed,
/// unsupported, or never-attempted query.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum QueryState<T> {
    #[default]
    NotQueried,
    Unsupported,
    Success(T),
    Failed {
        message: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NvmeHealth {
    pub critical_warning: u8,
    pub composite_temperature_kelvin: Option<u16>,
    pub available_spare_percent: Option<u8>,
    pub available_spare_threshold_percent: Option<u8>,
    /// NVMe Percentage Used is wear consumed (not wear remaining).
    pub percentage_used: Option<u8>,
    pub data_units_read: Option<u128>,
    pub data_units_written: Option<u128>,
    pub controller_busy_minutes: Option<u128>,
    pub power_cycles: Option<u128>,
    pub power_on_hours: Option<u128>,
    pub unsafe_shutdowns: Option<u128>,
    pub media_errors: Option<u128>,
    pub error_log_entries: Option<u128>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtaAttribute {
    pub id: u8,
    pub flags: u16,
    pub current: u8,
    pub worst: u8,
    /// Raw vendor-defined six-byte value, retained without assigning units.
    pub raw: [u8; 6],
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtaHealth {
    pub attributes: Vec<AtaAttribute>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Telemetry {
    pub nvme: QueryState<NvmeHealth>,
    pub ata: QueryState<AtaHealth>,
    /// Wear consumed, 0..=100. Only populate when the device/model mapping
    /// establishes the semantics; never infer it from an ATA attribute ID.
    pub endurance_percent: Option<f64>,
    pub power_on_hours: Option<u64>,
    pub temperature_c: Option<i16>,
    /// SMART/NVMe device-reported overall failure state.
    pub overall_failure: QueryState<bool>,
}

impl Telemetry {
    /// Populate common fields from a successfully parsed NVMe health log.
    pub fn apply_nvme(&mut self, health: NvmeHealth) {
        self.endurance_percent = health.percentage_used.map(|v| f64::from(v.min(100)));
        self.power_on_hours = health.power_on_hours.and_then(|v| u64::try_from(v).ok());
        self.temperature_c = health
            .composite_temperature_kelvin
            .and_then(|k| i16::try_from(i32::from(k) - 273).ok());
        let failed = health.critical_warning & ((1 << 0) | (1 << 2) | (1 << 3)) != 0;
        self.overall_failure = QueryState::Success(failed);
        self.nvme = QueryState::Success(health);
    }
}
