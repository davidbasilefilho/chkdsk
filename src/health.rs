//! Deterministic, conservative health scoring (higher is healthier).
//!
//! Endurance-used is scored as `100 - used` and never alone means failure.
//! Device-declared failures and pre-failure ATA threshold trips determine
//! Fail; low scores from wear or other noncritical signals remain Pass. ATA normalized values
//! use only device-declared thresholds and small normalized-point margin caps,
//! not a guessed vendor baseline or remaining-lifetime estimate.
//! Confidence describes evidence, not statistical certainty.

use crate::model::{AtaHealth, DiskKind, HealthWarningReason, NvmeHealth, QueryState, Telemetry};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthOutcome {
    Pass,
    Fail,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HealthScore {
    pub score: u8,
    pub outcome: HealthOutcome,
    pub confidence: f64,
    pub provenance: Vec<String>,
    /// Structured warning causes for UI presentation; absent in older JSON.
    #[serde(default)]
    pub warnings: Vec<HealthWarningReason>,
}

#[derive(Default)]
struct Evidence {
    available: bool,
    critical_failure: bool,
}

fn clamp_score(value: f64) -> u8 {
    value.round().clamp(0.0, 100.0) as u8
}

fn add_warning(warnings: &mut Vec<HealthWarningReason>, reason: HealthWarningReason) {
    if !warnings.contains(&reason) {
        warnings.push(reason);
    }
}

/// Return the same result for the same disk kind and telemetry. The disk kind
/// parameter is retained for the domain API; HDD score is still a snapshot,
/// never a wear/remaining-life forecast.
pub fn score_health(_kind: DiskKind, telemetry: &Telemetry) -> HealthScore {
    let mut score = 100.0;
    let mut cap = 100.0_f64;
    let mut confidence = 0.0_f64;
    let mut provenance = Vec::new();
    let mut warnings = Vec::new();
    let mut evidence = Evidence::default();

    if let QueryState::Success(nvme) = &telemetry.nvme {
        confidence += 0.65;
        evidence.available = true;
        provenance.push("NVMe SMART/Health log".to_owned());
        add_nvme_evidence(
            nvme,
            &mut score,
            &mut cap,
            &mut evidence.critical_failure,
            &mut provenance,
            &mut warnings,
        );
    }
    if !matches!(&telemetry.nvme, QueryState::Success(_))
        && let Some(used) = telemetry
            .endurance_percent
            .filter(|v| v.is_finite() && *v >= 0.0)
    {
        let used = used.min(100.0);
        score = score.min(100.0 - used);
        if used >= 100.0 {
            add_warning(&mut warnings, HealthWarningReason::EnduranceExhausted);
        }
        confidence += 0.20;
        evidence.available = true;
        provenance.push(if let Some(profile) = telemetry.ata_profile.as_deref() {
            format!("{profile} SMART attribute 202 raw lifetime used {used:.0}%")
        } else {
            "mapped endurance-used percentage".to_owned()
        });
    }
    if let QueryState::Success(true) = &telemetry.overall_failure {
        cap = cap.min(20.0);
        evidence.critical_failure = true;
        confidence += 0.35;
        evidence.available = true;
        provenance.push("device-reported overall failure".to_owned());
    } else if let QueryState::Success(false) = &telemetry.overall_failure {
        confidence += 0.10;
        evidence.available = true;
        provenance.push("device-reported no threshold failure (coarse only)".to_owned());
    }
    if let QueryState::Success(ata) = &telemetry.ata {
        add_ata_evidence(
            ata,
            &mut score,
            &mut cap,
            &mut confidence,
            &mut evidence,
            &mut provenance,
            &mut warnings,
        );
        if ata
            .attributes
            .iter()
            .any(|attribute| (1..=254).contains(&attribute.current))
        {
            confidence += 0.10;
        }
        provenance.push(format!(
            "{} ATA attributes retained (vendor raw values remain opaque)",
            ata.attributes.len()
        ));
    }
    if let Some(temp) = telemetry.temperature_c {
        confidence += 0.05;
        provenance.push(format!("temperature {temp}°C (informational)"));
    }
    if telemetry.nvme == QueryState::NotQueried
        && telemetry.ata == QueryState::NotQueried
        && matches!(&telemetry.overall_failure, QueryState::NotQueried)
        && telemetry.endurance_percent.is_none()
    {
        provenance.push("no health telemetry available".to_owned());
    }
    confidence = confidence.min(1.0);
    let score = if evidence.available {
        clamp_score(score.min(cap))
    } else {
        // Unknown telemetry is not a healthy 100% reading.
        0
    };
    let outcome = if !evidence.available {
        HealthOutcome::Unknown
    } else if evidence.critical_failure {
        HealthOutcome::Fail
    } else {
        HealthOutcome::Pass
    };
    HealthScore {
        score,
        outcome,
        confidence,
        provenance,
        warnings,
    }
}

fn add_ata_evidence(
    ata: &AtaHealth,
    score: &mut f64,
    cap: &mut f64,
    confidence: &mut f64,
    evidence: &mut Evidence,
    provenance: &mut Vec<String>,
    warnings: &mut Vec<HealthWarningReason>,
) {
    let mut threshold_evidence = false;
    let mut threshold_reached = false;
    let mut near_threshold = false;
    for attribute in &ata.attributes {
        // Generic 1..=254 range follows current smartctl documentation; vendor
        // exceptions require a verified profile. Retain unavailable bytes
        // without using them for threshold evidence or confidence.
        if !(1..=254).contains(&attribute.current) {
            continue;
        }
        let Some(threshold) = attribute.threshold.filter(|value| *value > 0) else {
            continue;
        };
        evidence.available = true;
        threshold_evidence = true;
        if attribute.current <= threshold {
            *cap = cap.min(20.0);
            threshold_reached = true;
            if attribute.flags & 1 != 0 {
                evidence.critical_failure = true;
            } else {
                add_warning(warnings, HealthWarningReason::AtaThresholdReached);
            }
            provenance.push(format!(
                "ATA SMART attribute {} current {} reached declared threshold {}{}",
                attribute.id,
                attribute.current,
                threshold,
                if attribute.flags & 1 != 0 {
                    " (pre-failure)"
                } else {
                    ""
                }
            ));
        }
        if attribute.current > threshold && attribute.current <= threshold.saturating_add(10) {
            near_threshold = true;
            add_warning(warnings, HealthWarningReason::AtaThresholdMargin);
            provenance.push(format!(
                "ATA SMART attribute {} is within 10 normalized points above threshold {}",
                attribute.id, threshold
            ));
        }
    }
    if threshold_evidence {
        *confidence += 0.15;
        if !threshold_reached && !near_threshold {
            provenance
                .push("ATA normalized currents remain above declared SMART thresholds".into());
        }
    }
    if near_threshold {
        *score = score.min(70.0);
        *confidence += 0.10;
    }
}

fn add_nvme_evidence(
    nvme: &NvmeHealth,
    score: &mut f64,
    cap: &mut f64,
    critical_failure: &mut bool,
    provenance: &mut Vec<String>,
    warnings: &mut Vec<HealthWarningReason>,
) {
    if let Some(used) = nvme.percentage_used {
        *score = score.min(f64::from(100_u8.saturating_sub(used.min(100))));
        if used >= 100 {
            add_warning(warnings, HealthWarningReason::EnduranceExhausted);
        }
        provenance.push(format!("NVMe endurance used {used}%"));
    }
    if let (Some(spare), Some(threshold)) = (
        nvme.available_spare_percent,
        nvme.available_spare_threshold_percent,
    ) {
        *score -= f64::from(100_u8.saturating_sub(spare.min(100))) * 0.30;
        provenance.push(format!(
            "NVMe available spare {spare}% (threshold {threshold}%)"
        ));
        if spare < threshold {
            *cap = cap.min(25.0);
            *critical_failure = true;
        }
    }
    let warning = nvme.critical_warning;
    if warning & 0b0000_0001 != 0 {
        *cap = cap.min(25.0);
        *critical_failure = true;
        provenance.push("NVMe spare critical warning".into());
    }
    if warning & 0b0000_0010 != 0 {
        *cap = cap.min(40.0);
        add_warning(warnings, HealthWarningReason::NvmeTemperatureThreshold);
        provenance.push("NVMe temperature critical warning".into());
    }
    if warning & 0b0000_0100 != 0 {
        *cap = cap.min(20.0);
        *critical_failure = true;
        provenance.push("NVMe reliability degraded warning".into());
    }
    if warning & 0b0000_1000 != 0 {
        *cap = cap.min(10.0);
        *critical_failure = true;
        provenance.push("NVMe media read-only warning".into());
    }
    if warning & 0b0001_0000 != 0 {
        *cap = cap.min(50.0);
        add_warning(warnings, HealthWarningReason::NvmeVolatileMemoryBackup);
        provenance.push("NVMe volatile-memory backup warning".into());
    }
    // Critical Warning bit 5 denotes a Persistent Memory Region that has
    // become read-only or unreliable (NVMe Base Spec 1.4a+). This optional
    // region warning is not treated as failure of the NVM media itself.
    if warning & 0b0010_0000 != 0 {
        *cap = cap.min(70.0);
        add_warning(warnings, HealthWarningReason::NvmePersistentMemoryRegion);
        provenance.push("NVMe persistent memory region read-only or unreliable".into());
    }
    if let Some(errors) = nvme.media_errors.filter(|count| *count > 0) {
        // Treat this historical counter as a warning, not instantaneous
        // failure: cap at 70, then subtract five points per decimal order,
        // bounded to a 30-point deduction.
        let penalty = (5.0 * (errors as f64).log10()).min(30.0);
        *cap = cap.min(70.0);
        *score = score.min(70.0) - penalty;
        add_warning(warnings, HealthWarningReason::NvmeMediaErrors);
        provenance.push(format!("NVMe media/data integrity errors: {errors}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AtaHealth, NvmeHealth};

    #[test]
    fn no_data_is_unknown_and_score_stays_in_range() {
        let result = score_health(DiskKind::Unknown, &Telemetry::default());
        assert_eq!(result.score, 0);
        assert_eq!(result.outcome, HealthOutcome::Unknown);
        assert_eq!(result.confidence, 0.0);
    }

    #[test]
    fn wear_is_deterministic_and_failure_caps_are_applied() {
        let telemetry = Telemetry {
            nvme: QueryState::Success(NvmeHealth {
                percentage_used: Some(80),
                critical_warning: 4,
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = score_health(DiskKind::Nvme, &telemetry);
        assert_eq!(result.score, 20);
        assert_eq!(result.outcome, HealthOutcome::Fail);
        assert_eq!(result, score_health(DiskKind::Nvme, &telemetry));
    }

    #[test]
    fn nonfailure_nvme_temperature_and_backup_warnings_do_not_mark_fail() {
        for (warning, expected_cap, reason) in [
            (
                0b0000_0010,
                40,
                HealthWarningReason::NvmeTemperatureThreshold,
            ),
            (
                0b0001_0000,
                50,
                HealthWarningReason::NvmeVolatileMemoryBackup,
            ),
            (
                0b0010_0000,
                70,
                HealthWarningReason::NvmePersistentMemoryRegion,
            ),
        ] {
            let telemetry = Telemetry {
                nvme: QueryState::Success(NvmeHealth {
                    critical_warning: warning,
                    ..Default::default()
                }),
                ..Default::default()
            };
            let result = score_health(DiskKind::Nvme, &telemetry);
            assert_eq!(result.score, expected_cap);
            assert_eq!(result.outcome, HealthOutcome::Pass);
            assert_eq!(result.warnings, vec![reason]);
        }
    }

    #[test]
    fn nvme_media_errors_warn_and_reduce_score_without_failing() {
        for (errors, expected_score) in [(0, 100), (1, 70), (100, 60), (u128::MAX, 40)] {
            let telemetry = Telemetry {
                nvme: QueryState::Success(NvmeHealth {
                    media_errors: Some(errors),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let result = score_health(DiskKind::Nvme, &telemetry);
            assert_eq!(result.score, expected_score, "{errors} errors");
            assert_eq!(result.outcome, HealthOutcome::Pass, "{errors} errors");
            assert_eq!(
                result.warnings,
                if errors == 0 {
                    vec![]
                } else {
                    vec![HealthWarningReason::NvmeMediaErrors]
                }
            );
        }
    }

    #[test]
    fn unmapped_ata_raw_ids_do_not_change_score() {
        let mut a = Telemetry::default();
        let before = score_health(DiskKind::Hdd, &a).score;
        a.ata = QueryState::Success(AtaHealth::default());
        assert_eq!(score_health(DiskKind::Hdd, &a).score, before);
        assert_eq!(
            score_health(DiskKind::Hdd, &a).outcome,
            HealthOutcome::Unknown
        );
    }

    #[test]
    fn coarse_no_failure_status_is_usable_but_low_confidence() {
        let telemetry = Telemetry {
            overall_failure: QueryState::Success(false),
            ..Default::default()
        };
        let result = score_health(DiskKind::Hdd, &telemetry);
        assert_eq!(result.score, 100);
        assert_eq!(result.outcome, HealthOutcome::Pass);
        assert_eq!(result.confidence, 0.10);
    }

    #[test]
    fn unavailable_ata_normalized_currents_do_not_supply_health_evidence() {
        for current in [0, 255] {
            let mut telemetry = Telemetry {
                ata: QueryState::Success(AtaHealth {
                    attributes: vec![crate::model::AtaAttribute {
                        id: 5,
                        flags: 1,
                        current,
                        worst: 1,
                        threshold: Some(10),
                        raw: [1, 2, 3, 4, 5, 6],
                    }],
                    thresholds_available: true,
                }),
                ..Default::default()
            };
            let retained = telemetry.clone();
            let result = score_health(DiskKind::Hdd, &telemetry);
            assert_eq!(result.outcome, HealthOutcome::Unknown, "current {current}");
            assert_eq!(result.score, 0);
            assert_eq!(result.confidence, 0.0);
            assert!(result.warnings.is_empty());
            assert_eq!(telemetry, retained);
            telemetry.overall_failure = QueryState::Success(false);
            let result = score_health(DiskKind::Hdd, &telemetry);
            assert_eq!(result.outcome, HealthOutcome::Pass);
            assert_eq!(result.score, 100);
            assert_eq!(result.confidence, 0.10);
            telemetry.overall_failure = QueryState::Success(true);
            let result = score_health(DiskKind::Hdd, &telemetry);
            assert_eq!(result.outcome, HealthOutcome::Fail);
            assert_eq!(result.score, 20);
            assert_eq!(result.confidence, 0.35);
        }
    }

    #[test]
    fn ata_normalized_currents_253_and_254_remain_valid_threshold_evidence() {
        for current in [253, 254] {
            let telemetry = Telemetry {
                ata: QueryState::Success(AtaHealth {
                    attributes: vec![crate::model::AtaAttribute {
                        id: 5,
                        flags: 1,
                        current,
                        worst: current,
                        threshold: Some(10),
                        raw: [0; 6],
                    }],
                    thresholds_available: true,
                }),
                ..Default::default()
            };
            let result = score_health(DiskKind::Hdd, &telemetry);
            assert_eq!(result.outcome, HealthOutcome::Pass, "current {current}");
            assert_eq!(result.score, 100);
            assert_eq!(result.confidence, 0.25);
        }
    }
    #[test]
    fn nvme_spare_threshold_equality_requires_a_device_critical_warning_to_fail() {
        for (spare, warning, expected_outcome, expected_score) in [
            (10, 0, HealthOutcome::Pass, 73),
            (9, 0, HealthOutcome::Fail, 25),
            (10, 1, HealthOutcome::Fail, 25),
            (11, 1, HealthOutcome::Fail, 25),
        ] {
            let telemetry = Telemetry {
                nvme: QueryState::Success(NvmeHealth {
                    available_spare_percent: Some(spare),
                    available_spare_threshold_percent: Some(10),
                    critical_warning: warning,
                    ..Default::default()
                }),
                ..Default::default()
            };
            let result = score_health(DiskKind::Nvme, &telemetry);
            assert_eq!(
                result.outcome, expected_outcome,
                "spare {spare}, warning {warning}"
            );
            assert_eq!(result.score, expected_score);
        }
    }
    #[test]
    fn declared_threshold_trip_caps_score_even_when_endurance_looks_good() {
        let telemetry = Telemetry {
            endurance_percent: Some(0.0),
            ata: QueryState::Success(AtaHealth {
                attributes: vec![crate::model::AtaAttribute {
                    id: 5,
                    flags: 1,
                    current: 10,
                    worst: 8,
                    threshold: Some(10),
                    raw: [0; 6],
                }],
                thresholds_available: true,
            }),
            ..Default::default()
        };
        let result = score_health(DiskKind::Ssd, &telemetry);
        assert!(result.score <= 20);
        assert_eq!(result.outcome, HealthOutcome::Fail);
        assert!(
            result
                .provenance
                .iter()
                .any(|item| item.contains("pre-failure"))
        );
    }

    #[test]
    fn old_age_endurance_threshold_reaches_zero_without_false_device_failure() {
        let telemetry = Telemetry {
            endurance_percent: Some(100.0),
            overall_failure: QueryState::Success(false),
            ata: QueryState::Success(AtaHealth {
                attributes: vec![crate::model::AtaAttribute {
                    id: 202,
                    flags: 0,
                    current: 1,
                    worst: 0,
                    threshold: Some(1),
                    raw: [100, 0, 0, 0, 0, 0],
                }],
                thresholds_available: true,
            }),
            ..Default::default()
        };
        let result = score_health(DiskKind::Ssd, &telemetry);
        assert_eq!(result.score, 0);
        assert_eq!(result.outcome, HealthOutcome::Pass);
        assert!(
            result
                .warnings
                .contains(&HealthWarningReason::EnduranceExhausted)
        );
        assert!(
            result
                .warnings
                .contains(&HealthWarningReason::AtaThresholdReached)
        );
        assert!(result.provenance.iter().any(|item| {
            item.contains("attribute 202") && item.contains("reached declared threshold")
        }));
        assert!(
            !result
                .provenance
                .iter()
                .any(|item| { item.contains("currents remain above declared SMART thresholds") })
        );
    }

    #[test]
    fn historical_worst_at_threshold_does_not_trip_a_current_passing_attribute() {
        let telemetry = Telemetry {
            ata: QueryState::Success(AtaHealth {
                attributes: vec![crate::model::AtaAttribute {
                    id: 197,
                    flags: 1,
                    current: 85,
                    worst: 8,
                    threshold: Some(10),
                    raw: [0; 6],
                }],
                thresholds_available: true,
            }),
            ..Default::default()
        };
        for kind in [DiskKind::Hdd, DiskKind::Ssd] {
            let result = score_health(kind, &telemetry);
            assert_eq!(result.score, 100);
            assert_eq!(result.outcome, HealthOutcome::Pass);
            assert!(
                !result
                    .provenance
                    .iter()
                    .any(|item| item.contains("reached declared threshold"))
            );
        }
    }

    #[test]
    fn normalized_values_near_but_above_threshold_get_a_small_low_confidence_cap() {
        let telemetry = Telemetry {
            ata: QueryState::Success(AtaHealth {
                attributes: vec![crate::model::AtaAttribute {
                    id: 1,
                    flags: 0,
                    current: 20,
                    worst: 20,
                    threshold: Some(10),
                    raw: [0; 6],
                }],
                thresholds_available: true,
            }),
            ..Default::default()
        };
        let result = score_health(DiskKind::Ssd, &telemetry);
        assert_eq!(result.score, 70);
        assert_eq!(result.outcome, HealthOutcome::Pass);
        assert_eq!(
            result.warnings,
            vec![HealthWarningReason::AtaThresholdMargin]
        );
        assert!(result.confidence < 0.5);
    }

    #[test]
    fn nvme_used_wear_hits_zero_at_100_and_101_without_false_failure() {
        for used in [100, 101] {
            let telemetry = Telemetry {
                nvme: QueryState::Success(NvmeHealth {
                    percentage_used: Some(used),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let result = score_health(DiskKind::Nvme, &telemetry);
            assert_eq!(result.score, 0);
            assert_eq!(result.outcome, HealthOutcome::Pass);
            assert_eq!(
                result.warnings,
                vec![HealthWarningReason::EnduranceExhausted]
            );
        }
        let telemetry = Telemetry {
            nvme: QueryState::Success(NvmeHealth {
                percentage_used: Some(12),
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = score_health(DiskKind::Nvme, &telemetry);
        assert_eq!(result.score, 88);
        assert_eq!(result.outcome, HealthOutcome::Pass);
    }

    #[test]
    fn mapped_sata_endurance_uses_all_wear_but_wear_alone_never_fails() {
        for (used, expected_score) in [(23.0, 77), (100.0, 0)] {
            let telemetry = Telemetry {
                endurance_percent: Some(used),
                ..Default::default()
            };
            let result = score_health(DiskKind::Ssd, &telemetry);
            assert_eq!(result.score, expected_score);
            assert_eq!(result.outcome, HealthOutcome::Pass);
        }
    }

    #[test]
    fn unprofiled_hdd_raw_sector_ids_do_not_invent_health_semantics() {
        for id in [5, 197, 198] {
            let telemetry = Telemetry {
                ata: QueryState::Success(AtaHealth {
                    attributes: vec![crate::model::AtaAttribute {
                        id,
                        flags: 0,
                        current: 100,
                        worst: 100,
                        threshold: None,
                        raw: [1, 0, 0, 0, 0, 0],
                    }],
                    thresholds_available: false,
                }),
                ..Default::default()
            };
            for kind in [DiskKind::Hdd, DiskKind::Ssd, DiskKind::Unknown] {
                let result = score_health(kind, &telemetry);
                assert_eq!(result.score, 0, "id {id}, kind {kind:?}");
                assert_eq!(result.outcome, HealthOutcome::Unknown, "id {id}");
            }
        }
    }

    #[test]
    fn old_health_score_json_defaults_structured_warnings() {
        let score: HealthScore = serde_json::from_str(
            r#"{"score":100,"outcome":"pass","confidence":0.1,"provenance":[]}"#,
        )
        .unwrap();
        assert!(score.warnings.is_empty());
    }
}
