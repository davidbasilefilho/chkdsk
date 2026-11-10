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
    AtaReallocatedSectors,
    AtaSpinRetries,
    AtaReportedUncorrectable,
    AtaPendingSectors,
    AtaOfflineUncorrectable,
    AtaRawEncodingAmbiguous,
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
    /// Parsed SMART rows. Duplicate IDs remain present for diagnosis, but
    /// scoring excludes the duplicated IDs as ambiguous evidence.
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
    /// Manufacturer/model profile used to decode or interpret ATA attributes.
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

    /// The disk's kind, filling a missing one from the telemetry itself: a valid
    /// NVMe log, or a verified ATA SSD wear profile. Discovery's explicit
    /// classification always wins, and a failed probe is not evidence.
    pub fn effective_kind(&self, identity: &DiskIdentity) -> DiskKind {
        if identity.kind != DiskKind::Unknown {
            return identity.kind;
        }
        if matches!(self.nvme, QueryState::Success(_)) {
            return DiskKind::Nvme;
        }
        if self.endurance_percent.is_some()
            && matches!(self.ata, QueryState::Success(_))
            && crate::ata::profile(identity.model.as_deref())
                .is_some_and(|profile| self.ata_profile.as_deref() == Some(profile.family))
        {
            return DiskKind::Ssd;
        }
        identity.kind
    }

    /// Apply ATA attributes and profile-backed raw-value semantics.
    pub fn apply_ata(&mut self, health: AtaHealth, model: Option<&str>) {
        let profile = crate::ata::profile(model);
        let hdd_profile = crate::ata::hdd_profile(model);
        let previous_ata_profile = self.ata_profile.is_some();
        self.ata_profile = profile
            .map(|profile| profile.family.to_owned())
            .or_else(|| hdd_profile.map(|profile| profile.family.to_owned()));
        self.power_on_hours = self
            .power_on_hours
            .or_else(|| crate::ata::hdd_power_on_hours(model, &health));
        self.temperature_c = self
            .temperature_c
            .or_else(|| crate::ata::hdd_temperature_c(model, &health));
        let endurance = profile
            .and_then(|profile| {
                crate::ata::unique_attribute(&health, profile.lifetime_used_attribute)
            })
            .and_then(|attribute| {
                crate::ata::endurance_used_percent(model, attribute.id, attribute.raw)
            })
            .map(f64::from);
        // Replace or clear prior ATA-derived wear when the model profile is
        // known. Never retain an arbitrary value from duplicated rows; preserve
        // an NVMe value if this telemetry also contains a valid NVMe log.
        if !matches!(&self.nvme, QueryState::Success(_))
            && (profile.is_some() || previous_ata_profile)
        {
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
    fn duplicate_crucial_lifetime_rows_do_not_choose_or_persist_an_arbitrary_wear_value() {
        for raw_values in [vec![23, 23], vec![23, 81], vec![81, 23]] {
            let health = AtaHealth {
                attributes: raw_values
                    .iter()
                    .map(|raw| AtaAttribute {
                        id: 202,
                        flags: 0,
                        current: 77,
                        worst: 75,
                        threshold: None,
                        raw: [*raw, 0, 0, 0, 0, 0],
                    })
                    .collect(),
                thresholds_available: false,
            };
            let mut telemetry = Telemetry {
                endurance_percent: Some(11.0),
                ata_profile: Some("Crucial MX500".to_owned()),
                ..Default::default()
            };
            telemetry.apply_ata(health.clone(), Some("Crucial CT500MX500SSD1"));

            assert_eq!(telemetry.endurance_percent, None, "rows {raw_values:?}");
            assert_eq!(telemetry.ata_profile.as_deref(), Some("Crucial MX500"));
            assert!(matches!(
                &telemetry.ata,
                QueryState::Success(parsed)
                    if parsed.attributes.len() == raw_values.len()
                        && parsed.attributes.iter().map(|row| row.raw[0]).collect::<Vec<_>>() == raw_values
            ));

            // The UI copies this telemetry value into the serialized history
            // sample. Ambiguous duplicates may remain diagnostic rows, but
            // they must not create an ETA baseline or persist invented wear.
            let mut history = crate::history::History::default();
            history
                .record(crate::history::HealthSample {
                    disk_id: "crucial-a".to_owned(),
                    timestamp_unix_secs: 1_000,
                    kind: DiskKind::Ssd,
                    endurance_percent: telemetry.endurance_percent,
                    power_on_hours: telemetry.power_on_hours,
                    score: None,
                })
                .unwrap();
            assert_eq!(history.samples[0].endurance_percent, None);
            assert_eq!(history.estimate_endurance("crucial-a"), None);
        }
    }

    #[test]
    fn duplicate_ata_lifetime_rows_do_not_overwrite_valid_nvme_wear() {
        let mut telemetry = Telemetry::default();
        telemetry.apply_nvme(NvmeHealth {
            percentage_used: Some(4),
            ..Default::default()
        });
        telemetry.apply_ata(
            AtaHealth {
                attributes: vec![
                    AtaAttribute {
                        id: 202,
                        flags: 0,
                        current: 77,
                        worst: 75,
                        threshold: None,
                        raw: [23, 0, 0, 0, 0, 0],
                    },
                    AtaAttribute {
                        id: 202,
                        flags: 0,
                        current: 77,
                        worst: 75,
                        threshold: None,
                        raw: [81, 0, 0, 0, 0, 0],
                    },
                ],
                thresholds_available: false,
            },
            Some("Crucial CT500MX500SSD1"),
        );
        assert_eq!(telemetry.endurance_percent, Some(4.0));
    }

    #[test]
    fn wd10ezex_profile_enables_hdd_signal_decoding_without_inventing_wear() {
        let mut telemetry = Telemetry::default();
        telemetry.apply_ata(
            AtaHealth {
                attributes: vec![
                    AtaAttribute {
                        id: 9,
                        flags: 0,
                        current: 100,
                        worst: 100,
                        threshold: None,
                        raw: [0x07, 0x1a, 0, 0, 0, 0],
                    },
                    AtaAttribute {
                        id: 194,
                        flags: 0,
                        current: 100,
                        worst: 100,
                        threshold: None,
                        raw: [40, 55, 63, 0, 0, 0],
                    },
                    AtaAttribute {
                        id: 197,
                        flags: 0,
                        current: 100,
                        worst: 100,
                        threshold: None,
                        raw: [0, 0, 1, 0, 0, 0],
                    },
                ],
                thresholds_available: false,
            },
            Some("WDC WD10EZEX-00WN4A0"),
        );
        assert_eq!(
            telemetry.ata_profile.as_deref(),
            Some(crate::ata::WD10EZEX_PROFILE)
        );
        assert_eq!(telemetry.endurance_percent, None);
        assert_eq!(telemetry.power_on_hours, Some(6663));
        assert_eq!(telemetry.temperature_c, Some(40));
        assert!(matches!(
            telemetry.ata,
            QueryState::Success(AtaHealth { ref attributes, .. })
                if attributes[2].raw == [0, 0, 1, 0, 0, 0]
        ));
    }

    #[test]
    fn older_ata_telemetry_json_defaults_new_threshold_fields() {
        let older =
            r#"{"attributes":[{"id":5,"flags":1,"current":100,"worst":100,"raw":[0,0,0,0,0,0]}]}"#;
        let health: AtaHealth = serde_json::from_str(older).unwrap();
        assert!(!health.thresholds_available);
        assert_eq!(health.attributes[0].threshold, None);
    }

    fn unknown_disk(model: Option<&str>) -> DiskIdentity {
        DiskIdentity {
            id: "disk".into(),
            index: 0,
            model: model.map(str::to_owned),
            serial: None,
            firmware: None,
            capacity_bytes: None,
            bus: None,
            kind: DiskKind::Unknown,
        }
    }

    #[test]
    fn nvme_log_restores_unknown_kind_without_requiring_optional_wear() {
        let mut identity = unknown_disk(None);
        let mut telemetry = Telemetry::default();
        telemetry.apply_nvme(NvmeHealth::default());
        assert_eq!(telemetry.endurance_percent, None);
        assert_eq!(telemetry.effective_kind(&identity), DiskKind::Nvme);

        // Discovery's explicit classification is preserved.
        for kind in [DiskKind::Hdd, DiskKind::Ssd, DiskKind::Nvme] {
            identity.kind = kind;
            assert_eq!(telemetry.effective_kind(&identity), kind);
        }

        // Failed probes and common wear alone do not establish the media kind.
        identity.kind = DiskKind::Unknown;
        for state in [
            QueryState::NotQueried,
            QueryState::Unsupported,
            QueryState::Failed {
                message: "invalid NVMe log".into(),
            },
        ] {
            telemetry.nvme = state;
            telemetry.endurance_percent = Some(12.0);
            assert_eq!(telemetry.effective_kind(&identity), DiskKind::Unknown);
        }
    }

    #[test]
    fn verified_ata_ssd_profile_restores_unknown_kind_but_unprofiled_models_do_not() {
        let health = AtaHealth {
            attributes: vec![AtaAttribute {
                id: 202,
                flags: 0,
                current: 77,
                worst: 75,
                threshold: None,
                raw: [23, 0, 0, 0, 0, 0],
            }],
            thresholds_available: false,
        };
        let mut crucial = Telemetry::default();
        crucial.apply_ata(health.clone(), Some("Crucial CT500MX500SSD1"));
        let identity = unknown_disk(Some("Crucial CT500MX500SSD1"));
        assert_eq!(crucial.effective_kind(&identity), DiskKind::Ssd);

        let mut other = Telemetry::default();
        other.apply_ata(health, Some("Samsung SSD 870 EVO"));
        let identity = unknown_disk(Some("Samsung SSD 870 EVO"));
        assert_eq!(other.effective_kind(&identity), DiskKind::Unknown);
    }
}
