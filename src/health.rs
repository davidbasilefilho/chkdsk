//! Deterministic, conservative health scoring (higher is healthier).
//!
//! Score begins at 100. NVMe wear consumes up to 60 points linearly at 100%
//! used; spare depletion consumes up to 30 points linearly. Device-declared
//! critical conditions impose hard caps. ATA vendor attributes are retained
//! for display but do not affect score without an explicit model mapping.
//! Pass/fail is deliberately coarse: <=50 is Fail, >50 with evidence is Pass,
//! and no usable health evidence is Unknown. Confidence describes evidence,
//! not statistical certainty.

use crate::model::{DiskKind, NvmeHealth, QueryState, Telemetry};
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
}

fn clamp_score(value: f64) -> u8 {
    value.round().clamp(0.0, 100.0) as u8
}

/// Return the same result for the same disk kind and telemetry. The disk kind
/// parameter is retained for the domain API; HDD score is still a snapshot,
/// never a wear/remaining-life forecast.
pub fn score_health(_kind: DiskKind, telemetry: &Telemetry) -> HealthScore {
    let mut score = 100.0;
    let mut cap = 100.0_f64;
    let mut confidence = 0.0_f64;
    let mut provenance = Vec::new();
    let mut evidence = false;

    if let QueryState::Success(nvme) = &telemetry.nvme {
        confidence += 0.65;
        evidence = true;
        provenance.push("NVMe SMART/Health log".to_owned());
        add_nvme_evidence(nvme, &mut score, &mut cap, &mut provenance);
    }
    if !matches!(&telemetry.nvme, QueryState::Success(_))
        && let Some(used) = telemetry
            .endurance_percent
            .filter(|v| v.is_finite() && *v >= 0.0)
    {
        let used = used.min(100.0);
        score -= used * 0.60;
        confidence += 0.20;
        evidence = true;
        provenance.push("mapped endurance-used percentage".to_owned());
    }
    if let QueryState::Success(true) = &telemetry.overall_failure {
        cap = cap.min(20.0);
        confidence += 0.35;
        evidence = true;
        provenance.push("device-reported overall failure".to_owned());
    } else if let QueryState::Success(false) = &telemetry.overall_failure {
        confidence += 0.25;
        evidence = true;
        provenance.push("device-reported no overall failure".to_owned());
    }
    if let QueryState::Success(ata) = &telemetry.ata {
        // A parsed page proves the query succeeded, but arbitrary ATA IDs/raw
        // bytes have no portable reliability semantics.
        confidence += 0.10;
        provenance.push(format!(
            "{} raw ATA attributes (not scored)",
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
    let score = clamp_score(score.min(cap));
    let outcome = if !evidence {
        HealthOutcome::Unknown
    } else if score <= 50 {
        HealthOutcome::Fail
    } else {
        HealthOutcome::Pass
    };
    HealthScore {
        score,
        outcome,
        confidence,
        provenance,
    }
}

fn add_nvme_evidence(
    nvme: &NvmeHealth,
    score: &mut f64,
    cap: &mut f64,
    provenance: &mut Vec<String>,
) {
    if let Some(used) = nvme.percentage_used {
        *score -= f64::from(used.min(100)) * 0.60;
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
        if spare <= threshold {
            *cap = cap.min(25.0);
        }
    }
    let warning = nvme.critical_warning;
    if warning & 0b0000_0001 != 0 {
        *cap = cap.min(25.0);
        provenance.push("NVMe spare critical warning".into());
    }
    if warning & 0b0000_0010 != 0 {
        *cap = cap.min(40.0);
        provenance.push("NVMe temperature critical warning".into());
    }
    if warning & 0b0000_0100 != 0 {
        *cap = cap.min(20.0);
        provenance.push("NVMe reliability degraded warning".into());
    }
    if warning & 0b0000_1000 != 0 {
        *cap = cap.min(10.0);
        provenance.push("NVMe media read-only warning".into());
    }
    if warning & 0b0001_0000 != 0 {
        *cap = cap.min(50.0);
        provenance.push("NVMe volatile-memory backup warning".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AtaHealth, NvmeHealth};

    #[test]
    fn no_data_is_unknown_and_score_stays_in_range() {
        let result = score_health(DiskKind::Unknown, &Telemetry::default());
        assert_eq!(result.score, 100);
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
}
