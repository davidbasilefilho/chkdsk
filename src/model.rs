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

/// Machine-readable non-failure health conditions for UI presentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthWarningReason {
    AtaThresholdMargin,
    AtaThresholdReached,
    EnduranceExhausted,
    NvmeMediaErrors,
    NvmeTemperatureThreshold,
    NvmeVolatileMemoryBackup,
    NvmePersistentMemoryRegion,
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
    /// NVMe SMART count of unrecovered media/data integrity errors.
    pub media_errors: Option<u128>,
    pub error_log_entries: Option<u128>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtaAttribute {
    pub id: u8,
    pub flags: u16,
    pub current: u8,
    pub worst: u8,
    /// Device-declared failure threshold from SMART READ THRESHOLDS, when
    /// available. Zero/absent thresholds are not interpreted as a trip.
    #[serde(default)]
    pub threshold: Option<u8>,
    /// Raw vendor-defined six-byte value, retained without assigning units.
    pub raw: [u8; 6],
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtaHealth {
    pub attributes: Vec<AtaAttribute>,
    /// Whether a valid SMART READ THRESHOLDS sector was received.
    #[serde(default)]
    pub thresholds_available: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Telemetry {
    pub nvme: QueryState<NvmeHealth>,
    pub ata: QueryState<AtaHealth>,
    /// Wear consumed, 0..=100. Only populate when the device/model mapping
    /// establishes the semantics; never infer it from an ATA attribute ID.
    pub endurance_percent: Option<f64>,
    /// Manufacturer/model profile that authorized ATA lifetime semantics.
    #[serde(default)]
    pub ata_profile: Option<String>,
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

    /// Apply ATA attributes and only profile-backed wear semantics.
    pub fn apply_ata(&mut self, health: AtaHealth, model: Option<&str>) {
        let profile = crate::ata::profile(model);
        self.ata_profile = profile.map(|profile| profile.family.to_owned());
        let endurance = profile.and_then(|profile| {
            health
                .attributes
                .iter()
                .find(|attribute| attribute.id == profile.lifetime_used_attribute)
                .and_then(|attribute| {
                    crate::ata::endurance_used_percent(model, attribute.id, attribute.raw)
                })
                .map(f64::from)
        });
        if endurance.is_some() {
            self.endurance_percent = endurance;
        }
        self.ata = QueryState::Success(health);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_scoped_ata_profiles_decode_crucial_raw_used_only() {
        let health = |id, raw| AtaHealth {
            attributes: vec![AtaAttribute {
                id,
                flags: 0,
                current: 77,
                worst: 75,
                threshold: Some(5),
                raw: [raw, 0, 0, 0, 0, 0],
            }],
            thresholds_available: true,
        };
        let mut crucial = Telemetry::default();
        crucial.apply_ata(health(202, 23), Some("Crucial CT500MX500SSD1"));
        assert_eq!(crucial.endurance_percent, Some(23.0));
        assert_eq!(crucial.ata_profile.as_deref(), Some("Crucial MX500"));
        assert!(matches!(
            &crucial.ata,
            QueryState::Success(health) if health.attributes[0].current == 77
        ));

        let mut four_tb = Telemetry::default();
        four_tb.apply_ata(health(202, 23), Some("ATA_CT4000MX500SSD1"));
        assert_eq!(four_tb.endurance_percent, Some(23.0));
        assert_eq!(four_tb.ata_profile.as_deref(), Some("Crucial MX500"));

        for (model, id) in [
            ("Samsung SSD 850 EVO", 177),
            ("Samsung SSD 860 EVO", 177),
            ("Samsung SSD 870 EVO", 177),
            ("CT500MX500SSD2", 202),
            ("MacroVIP MV240GB", 177),
        ] {
            let mut unprofiled = Telemetry::default();
            unprofiled.apply_ata(health(id, 23), Some(model));
            assert_eq!(unprofiled.endurance_percent, None, "{model}");
            assert_eq!(unprofiled.ata_profile, None, "{model}");
        }
    }

    #[test]
    fn older_ata_telemetry_json_defaults_new_threshold_fields() {
        let older =
            r#"{"attributes":[{"id":5,"flags":1,"current":100,"worst":100,"raw":[0,0,0,0,0,0]}]}"#;
        let health: AtaHealth = serde_json::from_str(older).unwrap();
        assert!(!health.thresholds_available);
        assert_eq!(health.attributes[0].threshold, None);
    }
}
