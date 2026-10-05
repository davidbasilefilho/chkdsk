//! Bounded, serializable health history and conservative SSD endurance estimates.

use crate::model::DiskKind;
use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, VecDeque};

pub const HISTORY_MAX_DISKS: usize = 16;
pub const HISTORY_RETENTION_SECS: u64 = 35 * 24 * 60 * 60;
pub const HISTORY_HOURLY_WINDOW_SECS: u64 = 7 * 24 * 60 * 60;
/// At most 169 hourly buckets for the recent week plus 29 daily buckets.
pub const HISTORY_MAX_SAMPLES_PER_DISK: usize = 198;
const HISTORY_MAX_SAMPLES_TOTAL: usize = HISTORY_MAX_DISKS * HISTORY_MAX_SAMPLES_PER_DISK;
// Bound temporary allocation when loading oversized/corrupt files, then keep
// the newest serialized tail before normal retention and disk caps are applied.
const HISTORY_MAX_DESERIALIZED_SAMPLES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HealthSample {
    pub disk_id: String,
    pub timestamp_unix_secs: u64,
    pub kind: DiskKind,
    /// Wear consumed (NVMe Percentage Used or a verified model-specific map).
    pub endurance_percent: Option<f64>,
    pub power_on_hours: Option<u64>,
    pub score: Option<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct History {
    pub version: u32,
    pub samples: Vec<HealthSample>,
}

fn deserialize_sample_tail<'de, D>(deserializer: D) -> Result<Vec<HealthSample>, D::Error>
where
    D: Deserializer<'de>,
{
    struct SampleTailVisitor;

    impl<'de> Visitor<'de> for SampleTailVisitor {
        type Value = Vec<HealthSample>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a sequence of disk health samples")
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut samples = VecDeque::with_capacity(HISTORY_MAX_DESERIALIZED_SAMPLES);
            while let Some(sample) = seq.next_element::<HealthSample>()? {
                if samples.len() == HISTORY_MAX_DESERIALIZED_SAMPLES {
                    samples.pop_front();
                }
                samples.push_back(sample);
            }
            Ok(samples.into_iter().collect())
        }
    }

    deserializer.deserialize_seq(SampleTailVisitor)
}

impl<'de> Deserialize<'de> for History {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct StoredHistory {
            #[serde(default)]
            version: u32,
            #[serde(default, deserialize_with = "deserialize_sample_tail")]
            samples: Vec<HealthSample>,
        }

        let stored = StoredHistory::deserialize(deserializer)?;
        let mut history = Self {
            version: stored.version,
            samples: stored.samples,
        };
        history.sanitize();
        Ok(history)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryError {
    EmptyDiskId,
    InvalidEndurance,
    ScoreOutOfRange,
    NonIncreasingTimestamp,
}

impl std::fmt::Display for HistoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::EmptyDiskId => "disk ID must not be empty",
            Self::InvalidEndurance => "endurance percentage must be finite and between 0 and 255",
            Self::ScoreOutOfRange => "health score must be between 0 and 100",
            Self::NonIncreasingTimestamp => "sample time must increase for this disk",
        })
    }
}
impl std::error::Error for HistoryError {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnduranceEstimate {
    /// Estimated seconds until 100% wear-used, not a guarantee of device life.
    pub remaining_seconds: u64,
    pub wear_percent_per_day: f64,
    pub samples_used: usize,
}

fn valid_sample(sample: &HealthSample) -> bool {
    !sample.disk_id.trim().is_empty()
        && !sample
            .endurance_percent
            .is_some_and(|p| !p.is_finite() || !(0.0..=255.0).contains(&p))
        && !sample.score.is_some_and(|score| score > 100)
}

impl History {
    /// Sanitize deserialized or externally modified data. Keeps at most 16
    /// disks, 35 days per disk, one sample per hour for the newest seven days,
    /// and one per day before that. Within a bucket the newest observation is
    /// retained, preserving a useful long-term wear trend at bounded size.
    pub fn sanitize(&mut self) {
        if self.version == 0 {
            self.version = 1;
        }
        let mut by_disk: BTreeMap<String, Vec<HealthSample>> = BTreeMap::new();
        for sample in std::mem::take(&mut self.samples) {
            if valid_sample(&sample) {
                by_disk
                    .entry(sample.disk_id.clone())
                    .or_default()
                    .push(sample);
            }
        }

        let mut disks: Vec<(String, Vec<HealthSample>)> = by_disk.into_iter().collect();
        disks.sort_by(|(id_a, samples_a), (id_b, samples_b)| {
            let latest_a = samples_a
                .iter()
                .map(|sample| sample.timestamp_unix_secs)
                .max()
                .unwrap_or(0);
            let latest_b = samples_b
                .iter()
                .map(|sample| sample.timestamp_unix_secs)
                .max()
                .unwrap_or(0);
            latest_b.cmp(&latest_a).then_with(|| id_a.cmp(id_b))
        });
        disks.truncate(HISTORY_MAX_DISKS);

        self.samples.clear();
        for (_, mut disk_samples) in disks {
            disk_samples.sort_by_key(|sample| sample.timestamp_unix_secs);
            let Some(latest) = disk_samples.last().map(|s| s.timestamp_unix_secs) else {
                continue;
            };
            let retention_cutoff = latest.saturating_sub(HISTORY_RETENTION_SECS);
            let hourly_cutoff = latest.saturating_sub(HISTORY_HOURLY_WINDOW_SECS);
            let mut buckets: BTreeMap<(u8, u64), HealthSample> = BTreeMap::new();
            for sample in disk_samples {
                if sample.timestamp_unix_secs < retention_cutoff {
                    continue;
                }
                let (resolution, bucket) = if sample.timestamp_unix_secs >= hourly_cutoff {
                    (0, sample.timestamp_unix_secs / 3_600)
                } else {
                    (1, sample.timestamp_unix_secs / 86_400)
                };
                // Sorted iteration means the last insert retains the newest
                // point in each bucket, including wear-counter changes.
                buckets.insert((resolution, bucket), sample);
            }
            self.samples.extend(buckets.into_values());
        }
        self.samples.sort_by(|a, b| {
            a.disk_id
                .cmp(&b.disk_id)
                .then_with(|| a.timestamp_unix_secs.cmp(&b.timestamp_unix_secs))
        });
        debug_assert!(self.samples.len() <= HISTORY_MAX_SAMPLES_TOTAL);
    }

    /// Append a validated observation. Timestamps must advance for each disk;
    /// repeated polling replaces that hour's sample rather than growing files
    /// every few seconds. Retention and disk caps are applied after every write.
    pub fn record(&mut self, sample: HealthSample) -> Result<(), HistoryError> {
        if sample.disk_id.trim().is_empty() {
            return Err(HistoryError::EmptyDiskId);
        }
        if sample
            .endurance_percent
            .is_some_and(|p| !p.is_finite() || !(0.0..=255.0).contains(&p))
        {
            return Err(HistoryError::InvalidEndurance);
        }
        if sample.score.is_some_and(|score| score > 100) {
            return Err(HistoryError::ScoreOutOfRange);
        }
        self.sanitize();
        if self
            .samples
            .iter()
            .filter(|existing| existing.disk_id == sample.disk_id)
            .map(|existing| existing.timestamp_unix_secs)
            .max()
            .is_some_and(|latest| sample.timestamp_unix_secs <= latest)
        {
            return Err(HistoryError::NonIncreasingTimestamp);
        }
        // A counter regression marks a new wear-counter epoch. Drop this
        // disk's earlier points before bucket coalescing, otherwise a reset
        // and a later same-hour reading can erase the reset boundary and
        // leave a misleading pre-reset baseline in persisted history.
        if let Some(wear) = sample.endurance_percent
            && self
                .samples
                .iter()
                .filter(|existing| existing.disk_id == sample.disk_id)
                .max_by_key(|existing| existing.timestamp_unix_secs)
                .and_then(|existing| existing.endurance_percent)
                .is_some_and(|previous| wear < previous)
        {
            self.samples
                .retain(|existing| existing.disk_id != sample.disk_id);
        }
        if self.version == 0 {
            self.version = 1;
        }
        self.samples.push(sample);
        self.sanitize();
        Ok(())
    }

    /// Estimate exhaustion from the oldest observation since the latest wear
    /// counter reset to the newest. Equal readings are retained because
    /// integer wear counters can stay unchanged across successive samples.
    /// No score-based or HDD forecast is made.
    pub fn estimate_endurance(&self, disk_id: &str) -> Option<EnduranceEstimate> {
        let mut points: Vec<&HealthSample> = self
            .samples
            .iter()
            .filter(|sample| {
                sample.disk_id == disk_id
                    && sample.kind != DiskKind::Hdd
                    && sample.kind != DiskKind::Unknown
            })
            .filter(|sample| {
                sample
                    .endurance_percent
                    .is_some_and(|p| p.is_finite() && (0.0..=255.0).contains(&p))
            })
            .collect();
        points.sort_by_key(|sample| sample.timestamp_unix_secs);
        let segment_start = points
            .windows(2)
            .rposition(|pair| {
                pair[1].endurance_percent.unwrap() < pair[0].endurance_percent.unwrap()
            })
            .map_or(0, |index| index + 1);
        let points = &points[segment_start..];
        let first = *points.first()?;
        let last = *points.last()?;
        let initial = first.endurance_percent?;
        let current = last.endurance_percent?;
        let elapsed = last
            .timestamp_unix_secs
            .checked_sub(first.timestamp_unix_secs)?;
        let increase = current - initial;
        // Rated endurance is exhausted at 100% wear-used. Report that endpoint
        // directly even when there is not enough history to establish a trend;
        // this is not a prediction of device failure.
        if current >= 100.0 {
            let wear_percent_per_day = if elapsed > 0 && increase > 0.0 {
                increase / elapsed as f64 * 86_400.0
            } else {
                0.0
            };
            return Some(EnduranceEstimate {
                remaining_seconds: 0,
                wear_percent_per_day,
                samples_used: points.len(),
            });
        }
        if elapsed == 0 {
            return None;
        }
        // Repeated or non-increasing readings do not establish a trend. A
        // regression was already handled as a new counter baseline above.
        if increase <= 0.0 {
            return None;
        }
        let rate_per_second = increase / elapsed as f64;
        let remaining = ((100.0 - current).max(0.0) / rate_per_second).ceil();
        if !remaining.is_finite() {
            return None;
        }
        let remaining_seconds = remaining.min(u64::MAX as f64) as u64;
        Some(EnduranceEstimate {
            remaining_seconds,
            wear_percent_per_day: rate_per_second * 86_400.0,
            samples_used: points.len(),
        })
    }

    /// Estimate the first power-on epoch from the earliest persisted sample
    /// with a device-reported power-on-hour counter. This is an age estimate,
    /// not a health or failure forecast.
    #[allow(dead_code)]
    pub fn estimated_first_power_on_unix_secs(&self, disk_id: &str) -> Option<u64> {
        self.samples
            .iter()
            .filter(|sample| sample.disk_id == disk_id)
            .filter_map(|sample| {
                let powered_secs = sample.power_on_hours?.checked_mul(3_600)?;
                Some((
                    sample.timestamp_unix_secs,
                    sample.timestamp_unix_secs.checked_sub(powered_secs)?,
                ))
            })
            .min_by_key(|(sample_time, _)| *sample_time)
            .map(|(_, estimated_start)| estimated_start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(
        disk_id: &str,
        timestamp: u64,
        kind: DiskKind,
        wear: Option<f64>,
        hours: Option<u64>,
    ) -> HealthSample {
        HealthSample {
            disk_id: disk_id.into(),
            timestamp_unix_secs: timestamp,
            kind,
            endurance_percent: wear,
            power_on_hours: hours,
            score: None,
        }
    }

    #[test]
    fn estimates_eta_across_retained_observations() {
        let mut history = History::default();
        history
            .record(sample("ssd-a", 1_000_000, DiskKind::Nvme, Some(10.0), None))
            .unwrap();
        history
            .record(sample("ssd-a", 1_086_400, DiskKind::Nvme, Some(11.0), None))
            .unwrap();
        history
            .record(sample("ssd-a", 1_172_800, DiskKind::Nvme, Some(12.0), None))
            .unwrap();
        let eta = history.estimate_endurance("ssd-a").unwrap();
        assert_eq!(eta.wear_percent_per_day, 1.0);
        assert_eq!(eta.remaining_seconds, 88 * 86_400);
        assert_eq!(eta.samples_used, 3);
        assert!(history.estimate_endurance("missing").is_none());
    }

    #[test]
    fn exhausted_rated_endurance_has_zero_eta_without_a_trend() {
        for wear in [100.0, 101.0, 255.0] {
            let mut history = History::default();
            history
                .record(sample("ssd-a", 1_000_000, DiskKind::Nvme, Some(wear), None))
                .unwrap();

            let estimate = history.estimate_endurance("ssd-a").unwrap();
            assert_eq!(estimate.remaining_seconds, 0, "wear={wear}");
            assert_eq!(estimate.wear_percent_per_day, 0.0, "wear={wear}");
            assert_eq!(estimate.samples_used, 1, "wear={wear}");
        }
    }

    #[test]
    fn repeated_quantized_wear_readings_preserve_a_long_trend() {
        let mut history = History::default();
        for (day, wear) in [(0, 10.0), (1, 10.0), (2, 11.0), (3, 11.0), (4, 12.0)] {
            history
                .record(sample(
                    "ssd-a",
                    1_000_000 + day * 86_400,
                    DiskKind::Nvme,
                    Some(wear),
                    None,
                ))
                .unwrap();
        }

        let estimate = history.estimate_endurance("ssd-a").unwrap();
        assert_eq!(estimate.wear_percent_per_day, 0.5);
        assert_eq!(estimate.samples_used, 5);
    }

    #[test]
    fn endurance_reset_starts_a_new_estimation_baseline() {
        let mut history = History::default();
        for (day, wear) in [
            (0, 10.0),
            (1, 12.0),
            (2, 0.0), // Counter reset: exclude the earlier baseline.
            (3, 0.0),
            (4, 1.0),
            (5, 1.0),
            (6, 3.0),
        ] {
            history
                .record(sample(
                    "ssd-a",
                    1_000_000 + day * 86_400,
                    DiskKind::Nvme,
                    Some(wear),
                    None,
                ))
                .unwrap();
        }

        let estimate = history.estimate_endurance("ssd-a").unwrap();
        assert_eq!(estimate.wear_percent_per_day, 0.75);
        assert_eq!(estimate.samples_used, 5);
    }

    #[test]
    fn reset_in_same_bucket_discards_old_epoch_across_round_trip() {
        let mut history = History::default();
        history
            .record(sample("ssd-a", 1_000_000, DiskKind::Nvme, Some(80.0), None))
            .unwrap();
        history
            .record(sample("ssd-a", 1_000_010, DiskKind::Nvme, Some(0.0), None))
            .unwrap();
        history
            .record(sample("ssd-a", 1_000_020, DiskKind::Nvme, Some(1.0), None))
            .unwrap();
        history
            .record(sample("ssd-a", 1_003_700, DiskKind::Nvme, Some(2.0), None))
            .unwrap();

        let restored: History =
            serde_json::from_str(&serde_json::to_string(&history).unwrap()).unwrap();
        assert!(
            restored.samples.iter().all(|point| {
                point.disk_id != "ssd-a" || point.endurance_percent.unwrap() < 80.0
            })
        );
        let estimate = restored.estimate_endurance("ssd-a").unwrap();
        assert_eq!(estimate.samples_used, 2);
        assert_eq!(estimate.wear_percent_per_day, 86_400.0 / 3_680.0);
    }

    #[test]
    fn hdd_does_not_receive_wear_eta_extrapolation() {
        let mut history = History::default();
        history
            .record(sample(
                "hdd-a",
                1_000_000,
                DiskKind::Hdd,
                Some(10.0),
                Some(100),
            ))
            .unwrap();
        history
            .record(sample(
                "hdd-a",
                1_000_100,
                DiskKind::Hdd,
                Some(20.0),
                Some(101),
            ))
            .unwrap();
        assert!(history.estimate_endurance("hdd-a").is_none());
        assert!(
            history
                .estimated_first_power_on_unix_secs("hdd-a")
                .is_some()
        );
    }

    #[test]
    fn power_on_age_and_json_round_trip() {
        let mut history = History::default();
        history
            .record(sample(
                "nvme-a",
                1_000_000,
                DiskKind::Nvme,
                Some(1.0),
                Some(100),
            ))
            .unwrap();
        assert_eq!(
            history.estimated_first_power_on_unix_secs("nvme-a"),
            Some(640_000)
        );
        let json = serde_json::to_string(&history).unwrap();
        assert_eq!(serde_json::from_str::<History>(&json).unwrap(), history);
    }

    #[test]
    fn hourly_sampling_bounds_frequent_polling_and_daily_sampling_keeps_weeks() {
        let mut history = History::default();
        let base = 2_000_000;
        for poll in 0..(24 * 60 * 60 / 15) {
            let second = (poll * 15) as u64;
            history
                .record(sample(
                    "ssd-a",
                    base + second,
                    DiskKind::Ssd,
                    Some(10.0 + (second / 3_600) as f64),
                    None,
                ))
                .unwrap();
        }
        assert!(history.samples.len() <= 25);

        for day in 1..35 {
            history
                .record(sample(
                    "ssd-a",
                    base + day * 86_400,
                    DiskKind::Ssd,
                    Some(35.0 + day as f64),
                    None,
                ))
                .unwrap();
        }
        assert!(history.samples.len() <= HISTORY_MAX_SAMPLES_PER_DISK);
        assert!(
            history
                .samples
                .iter()
                .any(|sample| sample.timestamp_unix_secs <= base + 7 * 86_400)
        );
        assert_eq!(
            history.estimate_endurance("ssd-a").unwrap().samples_used,
            history.samples.len()
        );
    }

    #[test]
    fn loaded_history_sanitizes_invalid_rows_and_caps_disks_and_samples() {
        let mut stored_samples = Vec::new();
        for disk in 0..(HISTORY_MAX_DISKS + 4) {
            for day in 0..50 {
                stored_samples.push(sample(
                    &format!("disk-{disk:02}"),
                    1_000_000 + disk as u64 * 100 + day as u64 * 86_400,
                    DiskKind::Ssd,
                    Some(day as f64),
                    None,
                ));
            }
        }
        stored_samples.push(sample(" ", 9_000_000, DiskKind::Ssd, Some(1.0), None));
        stored_samples.push(sample(
            "bad-score",
            9_000_001,
            DiskKind::Ssd,
            Some(1.0),
            None,
        ));
        stored_samples.last_mut().unwrap().score = Some(255);
        let json = serde_json::to_string(&serde_json::json!({
            "version": 1,
            "samples": stored_samples,
        }))
        .unwrap();
        let history: History = serde_json::from_str(&json).unwrap();
        assert!(history.samples.len() <= HISTORY_MAX_SAMPLES_TOTAL);
        assert!(history.samples.iter().all(|sample| {
            !sample.disk_id.trim().is_empty() && sample.score.is_none_or(|score| score <= 100)
        }));
        let distinct_disks: std::collections::BTreeSet<_> = history
            .samples
            .iter()
            .map(|sample| &sample.disk_id)
            .collect();
        assert!(distinct_disks.len() <= HISTORY_MAX_DISKS);
        for id in distinct_disks {
            assert!(
                history
                    .samples
                    .iter()
                    .filter(|sample| sample.disk_id == *id)
                    .count()
                    <= HISTORY_MAX_SAMPLES_PER_DISK
            );
        }
    }

    #[test]
    fn record_rejects_invalid_and_out_of_order_samples() {
        let mut history = History::default();
        assert_eq!(
            history.record(sample("x", 2, DiskKind::Ssd, Some(f64::NAN), None)),
            Err(HistoryError::InvalidEndurance)
        );
        history
            .record(sample("x", 2, DiskKind::Ssd, Some(10.0), None))
            .unwrap();
        assert_eq!(
            history.record(sample("x", 2, DiskKind::Ssd, Some(11.0), None)),
            Err(HistoryError::NonIncreasingTimestamp)
        );
    }
}
