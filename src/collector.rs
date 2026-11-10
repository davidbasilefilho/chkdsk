//! Coleta em segundo plano: descoberta periódica, consultas limitadas, histórico
//! em disco. Nada aqui toca a interface: o resultado chega como `Update`.

use crate::{
    backend::{self, QueryErrorState},
    health::{HealthOutcome, score_health},
    history::{HealthSample, History},
    model::{DiskIdentity, Telemetry},
};
use std::{
    collections::{HashMap, VecDeque},
    io::Write,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const DISCOVERY_INTERVAL: Duration = Duration::from_secs(15);
pub const MAX_CONCURRENT_DISK_QUERIES: usize = 4;

/// What the window knows about one disk: its identity and the latest outcome.
#[derive(Clone, Debug, PartialEq)]
pub struct DiskSnapshot {
    pub identity: DiskIdentity,
    pub reading: Reading,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Reading {
    /// No query has finished for this disk yet.
    Pending,
    Failed(QueryErrorState),
    Ready(Box<Ready>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ready {
    pub telemetry: Telemetry,
    /// Extrapolated seconds to 100% wear, from the history (never for HDDs).
    pub remaining_seconds: Option<u64>,
    pub last_sample_unix: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    Disks(Vec<DiskSnapshot>),
    DiscoveryFailed,
}

/// Telemetry plus what the history says about the disk.
pub fn ready_reading(identity: &DiskIdentity, telemetry: Telemetry, history: &History) -> Reading {
    Reading::Ready(Box::new(Ready {
        telemetry,
        remaining_seconds: history
            .estimate_endurance(&identity.id)
            .map(|estimate| estimate.remaining_seconds),
        last_sample_unix: history
            .samples
            .iter()
            .filter(|sample| sample.disk_id == identity.id)
            .map(|sample| sample.timestamp_unix_secs)
            .max(),
    }))
}

#[derive(Default)]
struct QueryScheduler {
    generation: u64,
    disks: Vec<DiskIdentity>,
    pending: VecDeque<DiskIdentity>,
    in_flight: HashMap<String, u64>,
}

impl QueryScheduler {
    fn update_disks(&mut self, mut disks: Vec<DiskIdentity>) {
        disks.sort_by(|left, right| left.id.cmp(&right.id));
        if self.disks != disks {
            self.generation = self.generation.saturating_add(1);
            self.disks = disks;
        }
        self.pending = self
            .disks
            .iter()
            .filter(|disk| !self.in_flight.contains_key(&disk.id))
            .cloned()
            .collect();
    }

    fn begin_next(&mut self) -> Option<(u64, DiskIdentity)> {
        if self.in_flight.len() >= MAX_CONCURRENT_DISK_QUERIES {
            return None;
        }
        while let Some(identity) = self.pending.pop_front() {
            if self.in_flight.contains_key(&identity.id) {
                continue;
            }
            self.in_flight.insert(identity.id.clone(), self.generation);
            return Some((self.generation, identity));
        }
        None
    }

    /// Retire the in-flight slot, accept only the current topology, and requeue
    /// a still-present disk if its result belonged to an older topology.
    fn finish(&mut self, generation: u64, identity: &DiskIdentity) -> bool {
        if self.in_flight.remove(&identity.id) != Some(generation) {
            return false;
        }
        let current = self.disks.iter().find(|disk| disk.id == identity.id);
        if generation == self.generation && current == Some(identity) {
            return true;
        }
        if let Some(current) = current
            && !self.pending.iter().any(|disk| disk.id == current.id)
        {
            self.pending.push_back(current.clone());
        }
        false
    }
}

/// The collector's state machine: no threads and no clock, so tests drive it directly.
struct Collector {
    history: History,
    scheduler: QueryScheduler,
    snapshots: Vec<DiskSnapshot>,
    writer: Option<Sender<History>>,
}

type QueryResult = Result<Telemetry, QueryErrorState>;

impl Collector {
    fn new(history: History, writer: Option<Sender<History>>) -> Self {
        Self {
            history,
            scheduler: QueryScheduler::default(),
            snapshots: Vec::new(),
            writer,
        }
    }

    /// A disk keeps its last reading while it is queried again.
    fn discovered(&mut self, disks: Vec<DiskIdentity>) -> Update {
        self.scheduler.update_disks(disks);
        let previous = std::mem::take(&mut self.snapshots);
        self.snapshots = self
            .scheduler
            .disks
            .iter()
            .map(|identity| DiskSnapshot {
                identity: identity.clone(),
                reading: previous
                    .iter()
                    .find(|snapshot| snapshot.identity == *identity)
                    .map_or(Reading::Pending, |snapshot| snapshot.reading.clone()),
            })
            .collect();
        Update::Disks(self.snapshots.clone())
    }

    /// `None` when the result belongs to an older topology.
    fn finished(
        &mut self,
        generation: u64,
        identity: &DiskIdentity,
        result: QueryResult,
    ) -> Option<Update> {
        if !self.scheduler.finish(generation, identity) {
            return None;
        }
        let reading = match result {
            Ok(telemetry) => {
                if record_reading(&mut self.history, identity, &telemetry)
                    && let Some(writer) = &self.writer
                {
                    let _ = writer.send(self.history.clone());
                }
                ready_reading(identity, telemetry, &self.history)
            }
            Err(state) => Reading::Failed(state),
        };
        let snapshot = self
            .snapshots
            .iter_mut()
            .find(|snapshot| snapshot.identity.id == identity.id)?;
        snapshot.reading = reading;
        Some(Update::Disks(self.snapshots.clone()))
    }
}

/// Append this reading to the history. False when the wall clock has not
/// advanced past the disk's newest sample, or the sample is rejected.
fn record_reading(history: &mut History, identity: &DiskIdentity, telemetry: &Telemetry) -> bool {
    let Some(timestamp_unix_secs) = next_timestamp(history, &identity.id) else {
        return false;
    };
    // Discovery may not know the kind; the telemetry can. Only the recorded
    // sample uses it, so the identity stays stable for topology comparisons.
    let kind = telemetry.effective_kind(identity);
    let score = score_health(kind, telemetry);
    let sample = HealthSample {
        disk_id: identity.id.clone(),
        timestamp_unix_secs,
        kind,
        endurance_percent: telemetry.endurance_percent,
        power_on_hours: telemetry.power_on_hours,
        score: (score.outcome != HealthOutcome::Unknown).then_some(score.score),
    };
    history.record(sample).is_ok()
}

fn next_timestamp(history: &History, disk_id: &str) -> Option<u64> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    next_timestamp_at(history, disk_id, now)
}

fn next_timestamp_at(history: &History, disk_id: &str, now: u64) -> Option<u64> {
    let latest = history
        .samples
        .iter()
        .filter(|sample| sample.disk_id == disk_id)
        .map(|sample| sample.timestamp_unix_secs)
        .max();
    match latest {
        Some(latest) if now <= latest => None,
        _ => Some(now),
    }
}

enum Message {
    Finished {
        generation: u64,
        identity: DiskIdentity,
        result: QueryResult,
    },
}

/// Start the collector thread. It sleeps between discoveries; `deliver` is
/// called from it with every change the window should show.
pub fn spawn(deliver: impl Fn(Update) + Send + 'static) {
    let _ = thread::Builder::new()
        .name("coleta".into())
        .spawn(move || run(history_path(), deliver));
}

fn run(path: PathBuf, deliver: impl Fn(Update)) {
    let history = load_history(&path);
    let mut collector = Collector::new(history, spawn_history_writer(path));
    let (sender, receiver) = mpsc::channel();
    let mut next_discovery = Instant::now();
    loop {
        match receiver.recv_timeout(next_discovery.saturating_duration_since(Instant::now())) {
            Ok(Message::Finished {
                generation,
                identity,
                result,
            }) => {
                if let Some(update) = collector.finished(generation, &identity, result) {
                    deliver(update);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                // Missed intervals are skipped, not replayed.
                next_discovery += DISCOVERY_INTERVAL;
                if next_discovery <= Instant::now() {
                    next_discovery = Instant::now() + DISCOVERY_INTERVAL;
                }
                deliver(discovery(&mut collector, backend::discover));
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
        start_queries(&mut collector.scheduler, &sender);
    }
}

/// A discovery that fails or panics is a notice; collection carries on.
fn discovery<E>(
    collector: &mut Collector,
    discover: impl FnOnce() -> Result<Vec<DiskIdentity>, E> + std::panic::UnwindSafe,
) -> Update {
    match std::panic::catch_unwind(discover) {
        Ok(Ok(disks)) => collector.discovered(disks),
        Ok(Err(_)) | Err(_) => Update::DiscoveryFailed,
    }
}

fn start_queries(scheduler: &mut QueryScheduler, sender: &Sender<Message>) {
    while let Some((generation, identity)) = scheduler.begin_next() {
        let (worker_sender, query_identity) = (sender.clone(), identity.clone());
        let started = thread::Builder::new()
            .name("consulta".into())
            .spawn(move || {
                // A panicking query must still free its slot.
                let result = std::panic::catch_unwind(|| {
                    backend::query(&query_identity).map_err(|error| error.state)
                })
                .unwrap_or(Err(QueryErrorState::Transient));
                let _ = worker_sender.send(Message::Finished {
                    generation,
                    identity: query_identity,
                    result,
                });
            });
        if started.is_err() {
            let _ = sender.send(Message::Finished {
                generation,
                identity,
                result: Err(QueryErrorState::Transient),
            });
        }
    }
}

pub fn history_path() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("chkdsk")
        .join("historico.json")
}

fn load_history(path: &Path) -> History {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<History>(&bytes).ok())
        .unwrap_or_default()
}

fn spawn_history_writer(path: PathBuf) -> Option<Sender<History>> {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("historico".into())
        .spawn(move || history_writer_loop(receiver, &path))
        .ok()?;
    Some(sender)
}

fn history_writer_loop(receiver: Receiver<History>, path: &Path) {
    while let Ok(mut newest) = receiver.recv() {
        // Snapshots are sent in order. Drain queued snapshots before each
        // write so a slow filesystem persists the newest state, never an older one.
        while let Ok(next) = receiver.try_recv() {
            newest = next;
        }
        let _ = write_history_snapshot(path, &newest);
    }
}

fn write_history_snapshot(path: &Path, history: &History) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let Some(file_name) = path.file_name() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "caminho do histórico sem nome de arquivo",
        ));
    };
    let mut temporary_name = file_name.to_os_string();
    temporary_name.push(format!(".tmp-{}", std::process::id()));
    let temporary = path.with_file_name(temporary_name);
    let write_result = (|| {
        let bytes = serde_json::to_vec(history).map_err(std::io::Error::other)?;
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write_result
}

#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    let replaced = unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING
                | windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DiskKind, NvmeHealth};

    fn identity(id: &str, index: u32) -> DiskIdentity {
        DiskIdentity {
            id: id.into(),
            index,
            model: None,
            serial: None,
            firmware: None,
            capacity_bytes: None,
            bus: None,
            kind: DiskKind::Unknown,
        }
    }

    #[test]
    fn wall_clock_rollback_holds_history_baseline_until_time_advances() {
        let mut history = History::default();
        history
            .record(HealthSample {
                disk_id: "ssd-a".into(),
                timestamp_unix_secs: 2_000,
                kind: DiskKind::Ssd,
                endurance_percent: Some(10.0),
                power_on_hours: None,
                score: Some(94),
            })
            .unwrap();

        assert_eq!(next_timestamp_at(&history, "ssd-a", 1_000), None);
        assert_eq!(next_timestamp_at(&history, "ssd-a", 2_000), None);
        assert_eq!(next_timestamp_at(&history, "ssd-a", 2_001), Some(2_001));
        assert_eq!(next_timestamp_at(&history, "other", 1_000), Some(1_000));
        assert_eq!(history.estimate_endurance("ssd-a"), None);
    }

    #[test]
    fn slow_queries_are_bounded_deduplicated_and_stale_hotplug_results_requeued() {
        let disks: Vec<DiskIdentity> = (0..8).map(|i| identity(&format!("disk-{i}"), i)).collect();
        let mut scheduler = QueryScheduler::default();
        scheduler.update_disks(disks.clone());
        let topology_generation = scheduler.generation;
        let first = (0..MAX_CONCURRENT_DISK_QUERIES)
            .map(|_| scheduler.begin_next().unwrap())
            .collect::<Vec<_>>();
        assert!(scheduler.begin_next().is_none());
        assert_eq!(scheduler.in_flight.len(), MAX_CONCURRENT_DISK_QUERIES);

        // Repeated polling cannot enqueue an already-running disk a second time.
        scheduler.update_disks(disks.clone());
        assert_eq!(scheduler.generation, topology_generation);
        assert_eq!(scheduler.in_flight.len(), MAX_CONCURRENT_DISK_QUERIES);
        assert!(
            scheduler
                .pending
                .iter()
                .all(|disk| !scheduler.in_flight.contains_key(&disk.id))
        );
        assert!(scheduler.finish(first[1].0, &first[1].1));

        let stale_identity = first[0].1.clone();
        let mut changed = disks;
        changed[0] = identity(&stale_identity.id, 99);
        scheduler.update_disks(changed);
        assert_ne!(scheduler.generation, topology_generation);
        assert!(!scheduler.finish(first[0].0, &stale_identity));
        assert!(
            scheduler
                .pending
                .iter()
                .any(|disk| disk.id == stale_identity.id && disk.index == 99)
        );
        assert!(scheduler.begin_next().is_some());
        assert!(scheduler.in_flight.len() <= MAX_CONCURRENT_DISK_QUERIES);
    }

    #[test]
    fn history_snapshots_are_serialized_atomic_and_end_at_the_newest_state() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "chkdsk-history-{}-{unique}.json",
            std::process::id()
        ));
        write_history_snapshot(
            &path,
            &History {
                version: 0,
                samples: Vec::new(),
            },
        )
        .unwrap();
        let writer = spawn_history_writer(path.clone()).unwrap();
        for version in 1..=5 {
            writer
                .send(History {
                    version,
                    samples: Vec::new(),
                })
                .unwrap();
        }
        // The writer exits once every sender is gone, after the last write.
        drop(writer);
        let deadline = Instant::now() + Duration::from_secs(10);
        let persisted = loop {
            let persisted = load_history(&path);
            if persisted.version == 5 || Instant::now() > deadline {
                break persisted;
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(persisted.version, 5);
        let temporary = path.with_file_name(format!(
            "{}.tmp-{}",
            path.file_name().unwrap().to_string_lossy(),
            std::process::id()
        ));
        assert!(!temporary.exists(), "arquivo temporário remanescente");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_disk_keeps_its_reading_while_queried_again_and_failures_clear_it() {
        let disk = identity("disk-a", 0);
        let mut collector = Collector::new(History::default(), None);
        let Update::Disks(first) = collector.discovered(vec![disk.clone()]) else {
            panic!("discovery must produce disks");
        };
        assert_eq!(first[0].reading, Reading::Pending);

        let (generation, queried) = collector.scheduler.begin_next().unwrap();
        let Some(Update::Disks(done)) =
            collector.finished(generation, &queried, Ok(Telemetry::default()))
        else {
            panic!("a current result must be delivered");
        };
        assert!(matches!(done[0].reading, Reading::Ready(_)));

        // Discovery again: still the same disk, the reading stays visible.
        let Update::Disks(again) = collector.discovered(vec![disk.clone()]) else {
            panic!("discovery must produce disks");
        };
        assert!(matches!(again[0].reading, Reading::Ready(_)));

        let (generation, queried) = collector.scheduler.begin_next().unwrap();
        let Some(Update::Disks(failed)) =
            collector.finished(generation, &queried, Err(QueryErrorState::Permission))
        else {
            panic!("a current failure must be delivered");
        };
        assert_eq!(
            failed[0].reading,
            Reading::Failed(QueryErrorState::Permission)
        );

        // A disk that disappears and returns changed starts over.
        let mut changed = disk;
        changed.index = 7;
        let Update::Disks(changed) = collector.discovered(vec![changed]) else {
            panic!("discovery must produce disks");
        };
        assert_eq!(changed[0].reading, Reading::Pending);
    }

    #[test]
    fn telemetry_restores_an_unknown_kind_for_history_and_the_estimate() {
        let disk = identity("nvme-fallback", 0);
        assert_eq!(disk.kind, DiskKind::Unknown);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut history = History::default();
        history
            .record(HealthSample {
                disk_id: disk.id.clone(),
                timestamp_unix_secs: now - 86_400,
                kind: DiskKind::Nvme,
                endurance_percent: Some(11.0),
                power_on_hours: None,
                score: Some(89),
            })
            .unwrap();
        let mut collector = Collector::new(history, None);
        collector.discovered(vec![disk.clone()]);
        let (generation, queried) = collector.scheduler.begin_next().unwrap();
        let mut telemetry = Telemetry::default();
        telemetry.apply_nvme(NvmeHealth {
            percentage_used: Some(12),
            ..Default::default()
        });
        let Some(Update::Disks(done)) = collector.finished(generation, &queried, Ok(telemetry))
        else {
            panic!("a current result must be delivered");
        };

        let sample = collector.history.samples.last().unwrap();
        assert_eq!(sample.kind, DiskKind::Nvme);
        assert_eq!(sample.endurance_percent, Some(12.0));
        assert_eq!(sample.score, Some(88));
        let Reading::Ready(ready) = &done[0].reading else {
            panic!("a reading must be ready");
        };
        assert!(ready.remaining_seconds.is_some());
        // Discovery identity stays stable for scheduler and hotplug comparisons.
        assert_eq!(done[0].identity, disk);
    }

    #[test]
    fn a_panicking_discovery_becomes_the_failure_notice_and_collection_continues() {
        let mut collector = Collector::new(History::default(), None);
        assert_eq!(
            discovery(&mut collector, || -> Result<Vec<DiskIdentity>, String> {
                panic!("driver exploded")
            }),
            Update::DiscoveryFailed
        );
        assert_eq!(
            discovery(&mut collector, || Err::<Vec<DiskIdentity>, _>("falhou")),
            Update::DiscoveryFailed
        );
        // The collector is intact: the next discovery works.
        let Update::Disks(disks) = discovery(&mut collector, || {
            Ok::<_, String>(vec![identity("disk-a", 0)])
        }) else {
            panic!("a later discovery must produce disks");
        };
        assert_eq!(disks.len(), 1);
    }

    #[test]
    fn a_stale_topology_result_is_dropped() {
        let mut collector = Collector::new(History::default(), None);
        collector.discovered(vec![identity("a", 0)]);
        let (generation, stale) = collector.scheduler.begin_next().unwrap();
        collector.discovered(vec![identity("a", 0), identity("b", 1)]);
        assert!(
            collector
                .finished(generation, &stale, Ok(Telemetry::default()))
                .is_none()
        );
    }
}
