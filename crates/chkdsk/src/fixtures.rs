//! Fixture disks for headless captures and tests: one healthy NVMe with a
//! lifetime estimate, one disk that needs attention, one whose health is
//! unknown because the query was denied, one still loading, one that failed,
//! one with too little data for a score, one that passes with soft warnings, a
//! generic SATA SSD with no wear attribute and a USB stick whose adapter does
//! not forward SMART.
//! `chkdsk --snap out.png --scene saudavel` renders them without a disk.

use crate::{
    collector::{DiskSnapshot, Reading, Update, ready_reading},
    ui::{Dashboard, Scene},
};
use chkdsk_core::{
    history::{HealthSample, History},
    model::{AtaAttribute, AtaHealth, DiskIdentity, DiskKind, NvmeHealth, QueryState, Telemetry},
};

/// Fixed clock so captures never change from one run to the next.
pub const NOW: u64 = 1_800_000_000;
const DAY: u64 = 86_400;

// Only the `scenes` feature puts it in the binary; tests build the same fixtures directly.
#[cfg_attr(not(feature = "scenes"), allow(dead_code))]
pub const SCENES: &[Scene] = &[
    ("saudavel", |s| *s = dashboard(0)),
    ("atencao", |s| *s = dashboard(1)),
    ("indisponivel", |s| *s = dashboard(2)),
    ("carregando", |s| *s = dashboard(3)),
    ("falha", |s| *s = dashboard(4)),
    ("dados-insuficientes", |s| *s = dashboard(5)),
    ("atencao-leve", |s| *s = dashboard(6)),
    ("ssd-generico", |s| *s = dashboard(7)),
    ("usb-sem-smart", |s| *s = dashboard(8)),
    ("procurando", |s| *s = Dashboard::default()),
    ("descoberta", |s| {
        *s = Dashboard::default();
        s.apply(Update::DiscoveryFailed, NOW);
    }),
    ("vazio", |s| {
        *s = Dashboard::default();
        s.apply(Update::Disks(Vec::new()), NOW);
    }),
];

/// The fixture disks, shown as the window would show them, with disk `selected` open.
pub fn dashboard(selected: usize) -> Dashboard {
    let mut dashboard = Dashboard::default();
    dashboard.apply(Update::Disks(disks()), NOW);
    dashboard.selected = selected;
    dashboard
}

pub fn identity(id: &str, model: &str, kind: DiskKind, bus: &str, bytes: u64) -> DiskIdentity {
    DiskIdentity {
        id: id.into(),
        index: 0,
        model: Some(model.into()),
        serial: None,
        firmware: None,
        capacity_bytes: Some(bytes),
        bus: Some(bus.into()),
        kind,
    }
}

pub fn history(disk: &DiskIdentity, samples: &[(u64, f64)]) -> History {
    let mut history = History::default();
    for &(days_ago, wear) in samples {
        history
            .record(HealthSample {
                disk_id: disk.id.clone(),
                timestamp_unix_secs: NOW - days_ago * DAY - DAY / 2,
                kind: disk.kind,
                endurance_percent: Some(wear),
                power_on_hours: None,
                score: None,
            })
            .expect("fixture samples advance in time");
    }
    history
}

pub fn disks() -> Vec<DiskSnapshot> {
    let nvme = identity(
        "interface:nvme-0",
        "Samsung SSD 990 PRO 1TB",
        DiskKind::Nvme,
        "NVMe",
        1_000_204_886_016,
    );
    let mut healthy = Telemetry::default();
    healthy.apply_nvme(NvmeHealth {
        composite_temperature_kelvin: Some(311),
        available_spare_percent: Some(100),
        available_spare_threshold_percent: Some(10),
        percentage_used: Some(7),
        power_on_hours: Some(4_380),
        ..Default::default()
    });
    let healthy_history = history(&nvme, &[(33, 5.0), (1, 7.0)]);

    let hdd = identity(
        "interface:sata-1",
        "WDC WD10EZEX-08WN4A0",
        DiskKind::Hdd,
        "SATA",
        1_000_204_886_016,
    );
    // Normalized values, declared thresholds and raw counts as a worn WD Blue reports them.
    let attributes = [
        (0x01, 100, 100, Some(51), 0),
        (0x03, 96, 94, Some(21), 0),
        (0x04, 100, 100, Some(0), 1_204),
        (0x05, 41, 41, Some(36), 1_736),
        (0x07, 100, 253, Some(0), 0),
        (0x09, 62, 62, Some(0), 33_815),
        (0x0A, 100, 100, Some(51), 0),
        (0x0C, 100, 100, Some(0), 1_204),
        (0xBB, 100, 100, Some(0), 0),
        (0xBE, 66, 55, None, 0x0022_0000_0022),
        (0xC2, 104, 98, None, 0x0014_0000_0022),
        (0xC4, 41, 41, Some(0), 1_736),
        (0xC5, 1, 1, Some(0), 24),
        (0xC6, 1, 1, Some(0), 24),
        (0xC7, 200, 200, Some(0), 0),
    ]
    .map(|(id, current, worst, threshold, raw)| AtaAttribute {
        id,
        flags: 0,
        current,
        worst,
        threshold,
        raw: u64::to_le_bytes(raw)[..6].try_into().expect("six bytes"),
    })
    .to_vec();
    // The model's profile decodes the hours and the temperature from the raw fields.
    let mut failing = Telemetry {
        overall_failure: QueryState::Success(true),
        ..Default::default()
    };
    failing.apply_ata(
        AtaHealth {
            attributes,
            thresholds_available: true,
        },
        hdd.model.as_deref(),
    );

    let ssd = identity(
        "interface:usb-2",
        "Crucial MX500 500GB",
        DiskKind::Ssd,
        "USB",
        500_107_862_016,
    );
    let denied = Telemetry {
        nvme: QueryState::Failed {
            message: "Access is denied (5)".into(),
        },
        ..Default::default()
    };
    let denied_history = history(&ssd, &[(30, 9.0), (2, 11.0)]);

    let pending = identity(
        "interface:nvme-3",
        "Kingston NV2 500GB",
        DiskKind::Nvme,
        "NVMe",
        500_107_862_016,
    );
    let failed = identity(
        "interface:usb-4",
        "Seagate Expansion Desk",
        DiskKind::Hdd,
        "USB",
        4_000_787_030_016,
    );

    // A temperature alone is usable data but no evidence of health: no score.
    let sparse = identity(
        "interface:sata-5",
        "Toshiba P300 2TB",
        DiskKind::Hdd,
        "SATA",
        2_000_398_934_016,
    );
    let sparse_telemetry = Telemetry {
        temperature_c: Some(38),
        ..Default::default()
    };

    // No device-declared failure, but media errors: passes with a soft warning.
    let worn = identity(
        "interface:nvme-6",
        "WD Blue SN570 1TB",
        DiskKind::Nvme,
        "NVMe",
        1_000_204_886_016,
    );
    let mut warned = Telemetry::default();
    warned.apply_nvme(NvmeHealth {
        composite_temperature_kelvin: Some(318),
        available_spare_percent: Some(100),
        available_spare_threshold_percent: Some(10),
        percentage_used: Some(21),
        power_on_hours: Some(9_120),
        media_errors: Some(12),
        ..Default::default()
    });

    // A generic SATA SSD as the MACROVIP MV240GB reports it: hours in 09,
    // cycles in 0C and no wear or temperature attribute at all.
    let generic = identity(
        "interface:sata-7",
        "MACROVIP MV240GB",
        DiskKind::Ssd,
        "SATA",
        240_057_409_536,
    );
    let generic_attributes = [
        (0x01, 100, 100, Some(50), 0),
        (0x05, 100, 100, Some(10), 0),
        (0x09, 100, 100, Some(0), 0x423C),
        (0x0C, 100, 100, Some(0), 0x832),
        (0xA0, 100, 100, Some(50), 0),
        (0xA1, 100, 100, Some(50), 0),
        (0xA3, 100, 100, Some(50), 0),
    ]
    .map(|(id, current, worst, threshold, raw)| AtaAttribute {
        id,
        flags: 0,
        current,
        worst,
        threshold,
        raw: u64::to_le_bytes(raw)[..6].try_into().expect("six bytes"),
    })
    .to_vec();
    let mut limited = Telemetry {
        overall_failure: QueryState::Success(false),
        ..Default::default()
    };
    limited.apply_ata(
        AtaHealth {
            attributes: generic_attributes,
            thresholds_available: true,
        },
        generic.model.as_deref(),
    );

    // A USB stick behind an adapter that answers no SMART command: identity only.
    let stick = identity(
        "interface:usb-8",
        "Lexar USB Flash Drive",
        DiskKind::Unknown,
        "USB",
        32_010_928_128,
    );

    vec![
        DiskSnapshot {
            reading: ready_reading(&nvme, healthy, &healthy_history),
            identity: nvme,
        },
        DiskSnapshot {
            reading: ready_reading(&hdd, failing, &History::default()),
            identity: hdd,
        },
        DiskSnapshot {
            reading: ready_reading(&ssd, denied, &denied_history),
            identity: ssd,
        },
        DiskSnapshot {
            reading: Reading::Pending,
            identity: pending,
        },
        DiskSnapshot {
            reading: Reading::Failed(chkdsk_windows::backend::QueryErrorState::Transient),
            identity: failed,
        },
        DiskSnapshot {
            reading: ready_reading(&sparse, sparse_telemetry, &History::default()),
            identity: sparse,
        },
        DiskSnapshot {
            reading: ready_reading(&worn, warned, &History::default()),
            identity: worn,
        },
        DiskSnapshot {
            reading: ready_reading(&generic, limited, &History::default()),
            identity: generic,
        },
        DiskSnapshot {
            reading: Reading::Failed(chkdsk_windows::backend::QueryErrorState::Unsupported),
            identity: stick,
        },
    ]
}
