//! Bounds-checked parsers for the fixed-size SMART health sectors.

use crate::model::{AtaAttribute, AtaHealth, NvmeHealth};

pub const HEALTH_LOG_SIZE: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub enum ParseError {
    InvalidLength { expected: usize, actual: usize },
    InvalidChecksum,
    InvalidValue(&'static str),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidLength { expected, actual } => {
                write!(f, "expected {expected}-byte sector, got {actual}")
            }
            Self::InvalidChecksum => f.write_str("ATA SMART data checksum is invalid"),
            Self::InvalidValue(reason) => f.write_str(reason),
        }
    }
}
impl std::error::Error for ParseError {}

fn require_sector(bytes: &[u8]) -> Result<(), ParseError> {
    if bytes.len() != HEALTH_LOG_SIZE {
        return Err(ParseError::InvalidLength {
            expected: HEALTH_LOG_SIZE,
            actual: bytes.len(),
        });
    }
    Ok(())
}

fn le128(bytes: &[u8]) -> u128 {
    let mut value = [0u8; 16];
    value.copy_from_slice(bytes);
    u128::from_le_bytes(value)
}

/// Parse an NVMe SMART / Health Information log (log page 02h, 512 bytes).
/// Temperatures remain in Kelvin here; translation into Celsius is exposed by
/// [`crate::model::Telemetry::apply_nvme`].
pub fn parse_nvme_health_log(bytes: &[u8]) -> Result<NvmeHealth, ParseError> {
    require_sector(bytes)?;
    let counter = |offset: usize| Some(le128(&bytes[offset..offset + 16]));
    let temperature = u16::from_le_bytes([bytes[1], bytes[2]]);
    let used = bytes[5]; // May exceed 100 after rated endurance is consumed.
    Ok(NvmeHealth {
        critical_warning: bytes[0],
        composite_temperature_kelvin: (temperature != 0).then_some(temperature),
        available_spare_percent: (bytes[3] <= 100).then_some(bytes[3]),
        available_spare_threshold_percent: (bytes[4] <= 100).then_some(bytes[4]),
        percentage_used: Some(used),
        data_units_read: counter(32),
        data_units_written: counter(48),
        controller_busy_minutes: counter(96),
        power_cycles: counter(112),
        power_on_hours: counter(128),
        unsafe_shutdowns: counter(144),
        media_errors: counter(160),
        error_log_entries: counter(176),
    })
}

/// Parse the 512-byte ATA SMART READ DATA sector, validating its checksum.
/// Duplicate attribute IDs are retained with their original six raw bytes;
/// consumers must treat the duplicated IDs as ambiguous rather than scoring
/// one twice or guessing which record is authoritative.
pub fn parse_ata_smart_data(bytes: &[u8]) -> Result<AtaHealth, ParseError> {
    require_sector(bytes)?;
    if bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) != 0 {
        return Err(ParseError::InvalidChecksum);
    }
    let mut attributes = Vec::new();
    for slot in 0..30 {
        let offset = 2 + slot * 12;
        let id = bytes[offset];
        if id == 0 {
            continue;
        }
        let raw: [u8; 6] = bytes[offset + 5..offset + 11]
            .try_into()
            .map_err(|_| ParseError::InvalidValue("malformed ATA attribute"))?;
        attributes.push(AtaAttribute {
            id,
            flags: u16::from_le_bytes([bytes[offset + 1], bytes[offset + 2]]),
            current: bytes[offset + 3],
            worst: bytes[offset + 4],
            threshold: None,
            raw,
        });
    }
    Ok(AtaHealth {
        attributes,
        thresholds_available: false,
    })
}

/// Parse the 512-byte ATA SMART READ THRESHOLDS sector and join declared
/// thresholds to matching READ DATA IDs. Attributes absent from the threshold
/// table keep `threshold: None`; no threshold is synthesized.
pub fn apply_ata_smart_thresholds(health: &mut AtaHealth, bytes: &[u8]) -> Result<(), ParseError> {
    require_sector(bytes)?;
    if bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) != 0 {
        return Err(ParseError::InvalidChecksum);
    }
    let mut seen_ids = [false; 256];
    let mut thresholds = Vec::new();
    for slot in 0..30 {
        let offset = 2 + slot * 12;
        let id = bytes[offset];
        if id == 0 {
            continue;
        }
        if seen_ids[usize::from(id)] {
            return Err(ParseError::InvalidValue("duplicate ATA SMART threshold ID"));
        }
        seen_ids[usize::from(id)] = true;
        thresholds.push((id, bytes[offset + 1]));
    }
    for (id, threshold) in thresholds {
        if let Some(attribute) = health.attributes.iter_mut().find(|item| item.id == id) {
            attribute.threshold = Some(threshold);
        }
    }
    health.thresholds_available = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{QueryState, Telemetry};

    fn finish_ata_checksum(fixture: &mut [u8; 512]) {
        fixture[511] = 0;
        fixture[511] = 0u8.wrapping_sub(
            fixture[..511]
                .iter()
                .fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
        );
    }

    // Small fixed-offset sectors model the NVMe log and ATA attribute table.
    #[test]
    fn parses_nvme_fixture_and_applies_common_telemetry() {
        let mut fixture = [0u8; 512];
        fixture[0] = 1; // spare below threshold
        fixture[1..3].copy_from_slice(&300u16.to_le_bytes());
        fixture[3] = 8;
        fixture[4] = 10;
        fixture[5] = 42;
        fixture[128..144].copy_from_slice(&1234u128.to_le_bytes());
        fixture[160..176].copy_from_slice(&7u128.to_le_bytes());
        let parsed = parse_nvme_health_log(&fixture).unwrap();
        assert_eq!(parsed.percentage_used, Some(42));
        assert_eq!(parsed.power_on_hours, Some(1234));
        assert_eq!(parsed.media_errors, Some(7));
        let mut telemetry = Telemetry::default();
        telemetry.apply_nvme(parsed);
        assert_eq!(telemetry.endurance_percent, Some(42.0));
        assert_eq!(telemetry.temperature_c, Some(27));
        assert_eq!(telemetry.overall_failure, QueryState::Success(true));
    }

    #[test]
    fn parses_ata_raw_attributes_without_assigning_vendor_semantics() {
        let mut fixture = [0u8; 512];
        let at = 2;
        fixture[at..at + 5].copy_from_slice(&[9, 0x32, 0, 99, 97]);
        fixture[at + 5..at + 11].copy_from_slice(&[0x78, 0x56, 0x34, 0x12, 0, 0]);
        finish_ata_checksum(&mut fixture);
        let parsed = parse_ata_smart_data(&fixture).unwrap();
        assert_eq!(parsed.attributes.len(), 1);
        assert_eq!(parsed.attributes[0].id, 9);
        assert_eq!(parsed.attributes[0].raw, [0x78, 0x56, 0x34, 0x12, 0, 0]);
        assert_eq!(parsed.attributes[0].current, 99);
        assert_eq!(parsed.attributes[0].threshold, None);
    }

    #[test]
    fn duplicate_smart_data_rows_are_preserved_but_not_scored_as_unique_evidence() {
        let mut data = [0u8; 512];
        data[2..7].copy_from_slice(&[197, 1, 0, 100, 100]);
        data[7..13].copy_from_slice(&[1, 0, 0, 0, 0, 0]);
        data[14..19].copy_from_slice(&[197, 1, 0, 99, 99]);
        data[19..25].copy_from_slice(&[100, 0, 0, 0, 0, 0]);
        finish_ata_checksum(&mut data);
        let health = parse_ata_smart_data(&data).unwrap();
        assert_eq!(health.attributes.len(), 2);
        assert_eq!(health.attributes[0].raw, [1, 0, 0, 0, 0, 0]);
        assert_eq!(health.attributes[1].raw, [100, 0, 0, 0, 0, 0]);
        let telemetry = Telemetry {
            ata: QueryState::Success(health),
            ata_profile: Some(crate::ata::WD10EZEX_PROFILE.to_owned()),
            ..Default::default()
        };
        let score = crate::health::score_health(crate::model::DiskKind::Hdd, &telemetry);
        assert_eq!(score.outcome, crate::health::HealthOutcome::Unknown);
        assert_eq!(score.score, 0);
        assert_eq!(score.confidence, 0.0);
        assert!(
            score
                .provenance
                .iter()
                .any(|item| item.contains("duplicate ATA attribute entries ignored"))
        );

        let mut clean = [0u8; 512];
        clean[2..7].copy_from_slice(&[197, 1, 0, 100, 100]);
        finish_ata_checksum(&mut clean);
        let mut health = parse_ata_smart_data(&clean).unwrap();
        let before = health.clone();

        let mut thresholds = [0u8; 512];
        thresholds[2..4].copy_from_slice(&[197, 10]);
        thresholds[14..16].copy_from_slice(&[197, 5]);
        finish_ata_checksum(&mut thresholds);
        assert_eq!(
            apply_ata_smart_thresholds(&mut health, &thresholds),
            Err(ParseError::InvalidValue("duplicate ATA SMART threshold ID"))
        );
        assert_eq!(
            health, before,
            "malformed optional sector must not partially mutate data"
        );
    }

    #[test]
    fn joins_only_declared_matching_thresholds_and_checks_both_checksums() {
        let mut data = [0u8; 512];
        data[2..7].copy_from_slice(&[5, 0x01, 0, 90, 70]);
        data[14..19].copy_from_slice(&[197, 0x01, 0, 95, 92]);
        finish_ata_checksum(&mut data);
        let mut health = parse_ata_smart_data(&data).unwrap();
        assert!(!health.thresholds_available);

        let mut thresholds = [0u8; 512];
        thresholds[2..4].copy_from_slice(&[5, 10]);
        thresholds[14..16].copy_from_slice(&[197, 0]);
        thresholds[26..28].copy_from_slice(&[198, 8]);
        finish_ata_checksum(&mut thresholds);
        apply_ata_smart_thresholds(&mut health, &thresholds).unwrap();
        assert!(health.thresholds_available);
        assert_eq!(health.attributes[0].threshold, Some(10));
        assert_eq!(health.attributes[1].threshold, Some(0));

        thresholds[70] ^= 1;
        assert_eq!(
            apply_ata_smart_thresholds(&mut health, &thresholds),
            Err(ParseError::InvalidChecksum)
        );
        assert_eq!(
            apply_ata_smart_thresholds(&mut health, &[0; 511]),
            Err(ParseError::InvalidLength {
                expected: 512,
                actual: 511
            })
        );
    }

    #[test]
    fn invalid_optional_threshold_sector_leaves_read_data_usable_but_unjoined() {
        let mut data = [0u8; 512];
        data[2..7].copy_from_slice(&[5, 1, 0, 100, 100]);
        finish_ata_checksum(&mut data);
        let mut health = parse_ata_smart_data(&data).unwrap();
        let mut thresholds = [0u8; 512];
        thresholds[2..4].copy_from_slice(&[5, 10]);
        finish_ata_checksum(&mut thresholds);
        thresholds[300] ^= 1;
        assert_eq!(
            apply_ata_smart_thresholds(&mut health, &thresholds),
            Err(ParseError::InvalidChecksum)
        );
        assert!(!health.thresholds_available);
        assert_eq!(health.attributes[0].threshold, None);
    }

    #[test]
    fn rejects_corrupt_ata_checksum() {
        let mut fixture = [0u8; 512];
        finish_ata_checksum(&mut fixture);
        fixture[42] ^= 1;
        assert_eq!(
            parse_ata_smart_data(&fixture),
            Err(ParseError::InvalidChecksum)
        );
    }

    #[test]
    fn rejects_non_512_byte_sector_and_preserves_over_100_wear() {
        assert!(matches!(
            parse_ata_smart_data(&[0; 511]),
            Err(ParseError::InvalidLength { .. })
        ));
        let mut fixture = [0u8; 512];
        fixture[5] = 127;
        assert_eq!(
            parse_nvme_health_log(&fixture).unwrap().percentage_used,
            Some(127)
        );
    }
}
