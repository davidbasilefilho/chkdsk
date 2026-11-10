//! Exportar os discos para um arquivo: JSON completo ou CSV de uma linha por
//! disco. A serialização é pura (testável sem janela); só o diálogo de salvar,
//! em `dialog`, depende do Windows.

use crate::{
    collector::{self, DiskSnapshot, Reading},
    present::{self, Body, Card, Tone},
};
use chkdsk_core::{
    history::History,
    model::{AtaAttribute, DiskKind, QueryState, Telemetry},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[cfg(windows)]
mod dialog;

/// Only Windows has the Save dialog. Tests and the `scenes` captures keep the
/// action on screen so the whole window can be seen and exercised.
pub const SUPPORTED: bool = cfg!(any(windows, test, feature = "scenes"));

/// Name offered in the Save dialog, without an extension: the chosen filter adds it.
pub const SUGGESTED_NAME: &str = "chkdsk-discos";

const BOM: &str = "\u{feff}";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Csv,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Format::Json => "json",
            Format::Csv => "csv",
        }
    }

    /// What the file name asks for: `.csv` is CSV, anything else is JSON.
    pub fn of_path(path: &Path) -> Format {
        match path.extension().and_then(|extension| extension.to_str()) {
            Some(extension) if extension.eq_ignore_ascii_case("csv") => Format::Csv,
            _ => Format::Json,
        }
    }
}

/// A file name that says its format: the one typed, or the chosen filter's
/// extension when none was typed.
pub fn with_extension(mut path: PathBuf, filter: Format) -> PathBuf {
    if path.extension().is_none() {
        path.set_extension(filter.extension());
    }
    path
}

/// What the Save dialog answered.
pub struct Pick {
    pub path: PathBuf,
    pub filter: Format,
}

/// Asks where to save, blocking until the user answers. `owner` is the window
/// handle the dialog sits over; none cancels.
pub type Picker = fn(owner: usize, suggested_name: &str) -> Option<Pick>;

#[cfg(windows)]
pub const PICKER: Option<Picker> = Some(dialog::pick);
#[cfg(not(windows))]
pub const PICKER: Option<Picker> = None;

/// The window the user just clicked in, which the dialog opens over.
#[cfg(windows)]
pub fn owner_window() -> usize {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    // SAFETY: plain query with no arguments.
    unsafe { GetForegroundWindow() as usize }
}

#[cfg(not(windows))]
pub fn owner_window() -> usize {
    0
}

/// What the export did, for the confirmation the window shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Saved {
        name: String,
    },
    /// The user closed the dialog: nothing to say.
    Cancelled,
    /// There is no Save dialog on this platform.
    Unavailable,
    /// The file could not be written.
    Failed,
}

/// Ask where, then write every disk there. Blocks on the dialog, so it runs on
/// its own thread.
pub fn run(
    snapshots: &[DiskSnapshot],
    now: u64,
    owner: usize,
    picker: Option<Picker>,
    history: impl FnOnce() -> History,
) -> Outcome {
    let Some(picker) = picker else {
        return Outcome::Unavailable;
    };
    let Some(pick) = picker(owner, SUGGESTED_NAME) else {
        return Outcome::Cancelled;
    };
    let path = with_extension(pick.path, pick.filter);
    let contents = match Format::of_path(&path) {
        Format::Json => to_json(snapshots, &history(), now),
        Format::Csv => to_csv(snapshots, now),
    };
    match std::fs::write(&path, contents) {
        Ok(()) => Outcome::Saved {
            name: path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into(),
            ),
        },
        Err(_) => Outcome::Failed,
    }
}

/// The history the collector keeps on disk, for the JSON export.
pub fn stored_history() -> History {
    collector::load_history(&collector::history_path())
}

fn tone_name(tone: Option<Tone>) -> Value {
    match tone {
        Some(Tone::Healthy) => "healthy".into(),
        Some(Tone::Caution) => "caution".into(),
        Some(Tone::Attention) => "attention".into(),
        Some(Tone::Unknown) => "unknown".into(),
        None => Value::Null,
    }
}

fn kind_name(kind: DiskKind) -> &'static str {
    match kind {
        DiskKind::Hdd => "hdd",
        DiskKind::Ssd => "ssd",
        DiskKind::Nvme => "nvme",
        DiskKind::Unknown => "unknown",
    }
}

fn effective_kind(snapshot: &DiskSnapshot) -> DiskKind {
    match &snapshot.reading {
        Reading::Ready(ready) => ready.telemetry.effective_kind(&snapshot.identity),
        Reading::Pending | Reading::Failed(_) => snapshot.identity.kind,
    }
}

fn attribute_json(attribute: &AtaAttribute) -> Value {
    json!({
        "id": attribute.id,
        "name": present::attribute_name(attribute.id),
        "flags": attribute.flags,
        "current": attribute.current,
        "worst": attribute.worst,
        "threshold": attribute.threshold,
        // Little-endian on the wire, written most significant first as the window shows it.
        "raw": attribute.raw.iter().rev().map(|byte| format!("{byte:02X}")).collect::<String>(),
    })
}

fn telemetry_json(telemetry: &Telemetry) -> Value {
    let nvme = match &telemetry.nvme {
        QueryState::Success(nvme) => serde_json::to_value(nvme).unwrap_or(Value::Null),
        _ => Value::Null,
    };
    let overall_failure = match &telemetry.overall_failure {
        QueryState::Success(failed) => Value::Bool(*failed),
        _ => Value::Null,
    };
    json!({
        "temperature_c": telemetry.temperature_c,
        "power_on_hours": telemetry.power_on_hours,
        "wear_percent": telemetry.endurance_percent,
        "device_reported_failure": overall_failure,
        "ata_profile": telemetry.ata_profile,
        "nvme_health_log": nvme,
    })
}

fn disk_json(snapshot: &DiskSnapshot, history: &History, now: u64) -> Value {
    let identity = &snapshot.identity;
    let card = Card::new(snapshot, now);
    let mut disk = json!({
        "id": identity.id,
        "model": identity.model,
        "serial": identity.serial,
        "firmware": identity.firmware,
        "bus": identity.bus,
        "kind": kind_name(effective_kind(snapshot)),
        "capacity_bytes": identity.capacity_bytes,
        "status": card.status_label(),
        "tone": tone_name(card.tone()),
        "score": Value::Null,
        "verdict": Value::Null,
        "warnings": [],
        "telemetry": Value::Null,
        "smart_attributes": [],
        "estimated_remaining_seconds": Value::Null,
        "last_reading_unix": Value::Null,
    });
    match (&card.body, &snapshot.reading) {
        (Body::Health(health), Reading::Ready(ready)) => {
            disk["score"] = health.score.into();
            disk["verdict"] = health.status.into();
            disk["warnings"] = health.warnings.clone().into();
            disk["telemetry"] = telemetry_json(&ready.telemetry);
            if let QueryState::Success(ata) = &ready.telemetry.ata {
                disk["smart_attributes"] = ata.attributes.iter().map(attribute_json).collect();
            }
            disk["estimated_remaining_seconds"] = ready.remaining_seconds.into();
            disk["last_reading_unix"] = ready.last_sample_unix.into();
        }
        (Body::Message(message), _) => disk["verdict"] = message.clone().into(),
        _ => {}
    }
    disk["history"] = history
        .samples
        .iter()
        .filter(|sample| sample.disk_id == identity.id)
        .filter_map(|sample| serde_json::to_value(sample).ok())
        .collect();
    disk
}

/// Every disk with what the window knows of it, and the stored history samples.
pub fn to_json(snapshots: &[DiskSnapshot], history: &History, now: u64) -> String {
    let document = json!({
        "app": "chkdsk",
        "version": env!("CARGO_PKG_VERSION"),
        "exported_at": iso8601_utc(now),
        "exported_at_unix": now,
        "disks": snapshots
            .iter()
            .map(|snapshot| disk_json(snapshot, history, now))
            .collect::<Vec<_>>(),
    });
    let mut text = serde_json::to_string_pretty(&document).unwrap_or_default();
    text.push('\n');
    text
}

const CSV_HEADER: [&str; 9] = [
    "model",
    "kind",
    "capacity_bytes",
    "score_percent",
    "status",
    "wear_percent",
    "temperature_c",
    "power_on_hours",
    "last_reading_utc",
];

/// One row per disk, comma separated and quoted as RFC 4180 asks, CRLF between
/// rows, with the byte order mark Excel needs to read UTF-8.
pub fn to_csv(snapshots: &[DiskSnapshot], now: u64) -> String {
    let mut out = String::from(BOM);
    out.push_str(&CSV_HEADER.join(","));
    out.push_str("\r\n");
    for snapshot in snapshots {
        let identity = &snapshot.identity;
        let card = Card::new(snapshot, now);
        let (telemetry, last_reading) = match &snapshot.reading {
            Reading::Ready(ready) => (Some(&ready.telemetry), ready.last_sample_unix),
            Reading::Pending | Reading::Failed(_) => (None, None),
        };
        let score = match &card.body {
            Body::Health(health) => health.score.map(|score| score.to_string()),
            _ => None,
        };
        let row = [
            text_field(identity.model.as_deref().unwrap_or("")),
            kind_name(effective_kind(snapshot)).to_owned(),
            identity
                .capacity_bytes
                .map(|bytes| bytes.to_string())
                .unwrap_or_default(),
            score.unwrap_or_default(),
            text_field(card.status_label()),
            telemetry
                .and_then(|telemetry| telemetry.endurance_percent)
                .map(|wear| wear.to_string())
                .unwrap_or_default(),
            telemetry
                .and_then(|telemetry| telemetry.temperature_c)
                .map(|value| value.to_string())
                .unwrap_or_default(),
            telemetry
                .and_then(|telemetry| telemetry.power_on_hours)
                .map(|hours| hours.to_string())
                .unwrap_or_default(),
            last_reading.map(iso8601_utc).unwrap_or_default(),
        ];
        out.push_str(&row.join(","));
        out.push_str("\r\n");
    }
    out
}

/// A text cell: a model name comes from the device, so a spreadsheet must not
/// read it as a formula; then quoted when it holds a comma, quote or line break.
fn text_field(text: &str) -> String {
    let text = text.trim();
    let guarded = if text.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{text}")
    } else {
        text.to_owned()
    };
    quote(&guarded)
}

/// RFC 4180: a field with a comma, quote or line break is wrapped in quotes and
/// its quotes doubled.
fn quote(field: &str) -> String {
    if field.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_owned()
    }
}

/// `2027-01-15T08:00:00Z` for Unix seconds.
pub fn iso8601_utc(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let seconds = unix % 86_400;
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3_600,
        seconds % 3_600 / 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests;
