//! Texto em português brasileiro. A tradução fica nesta fronteira: detalhes e
//! mensagens do sistema operacional nunca são exibidos diretamente. Os cartões
//! são montados quando um resultado chega, para que a janela só desenhe.

use crate::{
    backend::QueryErrorState,
    collector::{DISCOVERY_INTERVAL, DiskSnapshot, Reading, Ready},
    health::{HealthOutcome, score_health},
    model::{AtaAttribute, DiskKind, QueryState, Telemetry},
};
use std::time::Duration;

/// The three colors of the palette: healthy, attention, no usable data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Healthy,
    Attention,
    Critical,
    Unknown,
}

/// Everything the window shows about one disk, already formatted.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub id: String,
    /// Name in the disk list.
    pub list_name: String,
    /// Title of the detail panel.
    pub title: String,
    pub subtitle: String,
    pub body: Body,
    unsupported: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    /// First query still running.
    Loading,
    /// No telemetry: why.
    Message(String),
    Health(Box<Health>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Health {
    pub tone: Tone,
    pub label: &'static str,
    /// `93`; none when there is no usable evidence.
    pub score: Option<String>,
    /// `412 dias` or `aguardando histórico`.
    pub lifetime: String,
    /// The lifetime is an estimate, not the waiting placeholder.
    pub lifetime_known: bool,
    pub status: &'static str,
    /// How loudly the status line speaks; none for a plain remark.
    pub status_tone: Option<Tone>,
    pub wear: String,
    pub temperature: String,
    pub power_on: String,
    pub last_reading: String,
    pub warnings: Vec<&'static str>,
    pub attributes: Vec<AttributeRow>,
    pub details: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributeRow {
    pub id: String,
    pub current: String,
    pub worst: String,
    pub raw: String,
}

impl Card {
    /// `now` is Unix seconds, for "Última leitura há …".
    pub fn new(snapshot: &DiskSnapshot, now: u64) -> Self {
        let identity = &snapshot.identity;
        let model = identity
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty());
        let kind = match identity.kind {
            DiskKind::Hdd => crate::locale::tr("HD", "HDD"),
            DiskKind::Ssd => "SSD",
            DiskKind::Nvme => "NVMe",
            DiskKind::Unknown => crate::locale::tr("Tipo desconhecido", "Unknown type"),
        };
        let capacity = identity
            .capacity_bytes
            .map(format_bytes)
            .unwrap_or_else(|| {
                crate::locale::tr("capacidade indisponível", "capacity unavailable").into()
            });
        let body = match &snapshot.reading {
            Reading::Pending => Body::Loading,
            Reading::Failed(state) => Body::Message(query_error_message(*state)),
            Reading::Ready(ready) => Body::Health(Box::new(Health::new(identity, ready, now))),
        };
        Card {
            id: identity.id.clone(),
            list_name: model.unwrap_or(crate::locale::tr("Disco", "Drive")).into(),
            title: model
                .unwrap_or(crate::locale::tr("Modelo desconhecido", "Unknown model"))
                .into(),
            subtitle: format!("{kind}  ·  {capacity}"),
            unsupported: matches!(
                &snapshot.reading,
                Reading::Failed(QueryErrorState::Unsupported)
            ),
            body,
        }
    }

    /// The word next to the disk in the list, so color is never the only signal.
    pub fn status_label(&self) -> &'static str {
        match &self.body {
            Body::Loading => crate::locale::tr("Lendo…", "Reading…"),
            Body::Message(_) if self.unsupported => {
                crate::locale::tr("Não suportado", "Unsupported")
            }
            Body::Message(_) => crate::locale::tr("Indisponível", "Unavailable"),
            Body::Health(health) => health.label,
        }
    }

    /// Color of the disk's dot in the list; none until there is a reading.
    pub fn tone(&self) -> Option<Tone> {
        match &self.body {
            Body::Loading => None,
            Body::Message(_) => Some(Tone::Unknown),
            Body::Health(health) => Some(health.tone),
        }
    }
}

impl Health {
    fn new(identity: &crate::model::DiskIdentity, ready: &Ready, now: u64) -> Self {
        let telemetry = &ready.telemetry;
        let health = score_health(identity.kind, telemetry);
        let (label, tone) = match health.outcome {
            HealthOutcome::Pass => (crate::locale::tr("Saudável", "Healthy"), Tone::Healthy),
            HealthOutcome::Fail
                if matches!(telemetry.overall_failure, QueryState::Success(true)) =>
            {
                (crate::locale::tr("Crítico", "Critical"), Tone::Critical)
            }
            HealthOutcome::Fail => (crate::locale::tr("Atenção", "Attention"), Tone::Attention),
            HealthOutcome::Unknown => (
                crate::locale::tr("Indisponível", "Unavailable"),
                Tone::Unknown,
            ),
        };
        let score = telemetry
            .endurance_percent
            .filter(|n| n.is_finite() && *n >= 0.0)
            .map(|used| format!("{:.0}%", (100.0 - used).max(0.0)));
        let warnings = match &telemetry.nvme {
            QueryState::Success(nvme) => [
                (
                    1,
                    crate::locale::tr("Reserva NVMe crítica", "NVMe spare capacity critical"),
                ),
                (
                    2,
                    crate::locale::tr("Temperatura NVMe crítica", "NVMe temperature critical"),
                ),
                (
                    4,
                    crate::locale::tr("Confiabilidade NVMe degradada", "NVMe reliability degraded"),
                ),
                (
                    8,
                    crate::locale::tr("Mídia NVMe somente leitura", "NVMe media is read-only"),
                ),
                (
                    16,
                    crate::locale::tr("Aviso de memória NVMe", "NVMe volatile memory warning"),
                ),
            ]
            .into_iter()
            .filter(|(bit, _)| nvme.critical_warning & bit != 0)
            .map(|(_, warning)| warning)
            .collect(),
            _ => Vec::new(),
        };
        let mut details = Vec::new();
        let mut field = |name: &str, value: Option<String>| {
            if let Some(value) = value {
                details.push((name.to_owned(), value));
            }
        };
        field(crate::locale::tr("Modelo", "Model"), identity.model.clone());
        field("Firmware", identity.firmware.clone());
        field(crate::locale::tr("Barramento", "Bus"), identity.bus.clone());
        if let QueryState::Success(n) = &telemetry.nvme {
            field(
                crate::locale::tr("Fonte", "Source"),
                Some("NVMe SMART / Health Information".into()),
            );
            field(
                "NVMe Percentage Used",
                n.percentage_used.map(|v| format!("{v}%")),
            );
            field(
                crate::locale::tr("Tempo ligado NVMe", "NVMe power-on time"),
                n.power_on_hours.map(|v| format!("{v} h")),
            );
            for (key, value) in [
                (
                    crate::locale::tr(
                        "Unidades lidas (512.000 bytes)",
                        "Units read (512,000 bytes)",
                    ),
                    n.data_units_read,
                ),
                (
                    crate::locale::tr(
                        "Unidades gravadas (512.000 bytes)",
                        "Units written (512,000 bytes)",
                    ),
                    n.data_units_written,
                ),
                (
                    crate::locale::tr("Ciclos de energia", "Power cycles"),
                    n.power_cycles,
                ),
                (
                    crate::locale::tr("Erros de mídia", "Media errors"),
                    n.media_errors,
                ),
                (
                    crate::locale::tr("Entradas no log de erros", "Error log entries"),
                    n.error_log_entries,
                ),
            ] {
                field(key, value.map(|v| v.to_string()));
            }
        }
        if matches!(telemetry.ata, QueryState::Success(_)) {
            field(
                crate::locale::tr("Fonte", "Source"),
                Some("ATA SMART READ DATA".into()),
            );
        }
        if let Some(counters) = &telemetry.reliability {
            field(
                crate::locale::tr("Fonte", "Source"),
                Some("Windows StorageReliabilityCounter".into()),
            );
            field(
                "Windows Wear",
                counters.wear.filter(|v| *v <= 100).map(|v| format!("{v}%")),
            );
            field(
                "Windows Temperature",
                counters.temperature.map(|v| format!("{v} °C")),
            );
            field(
                "Windows PowerOnHours",
                counters.power_on_hours.map(|v| format!("{v} h")),
            );
            field(
                crate::locale::tr("Erros de leitura não corrigidos", "Uncorrected read errors"),
                counters.read_errors_uncorrected.map(|v| v.to_string()),
            );
            field(
                crate::locale::tr(
                    "Erros de gravação não corrigidos",
                    "Uncorrected write errors",
                ),
                counters.write_errors_uncorrected.map(|v| v.to_string()),
            );
        }
        Health {
            tone,
            label,
            score,
            lifetime: ready
                .remaining_seconds
                .map(format_endurance_days)
                .unwrap_or_else(|| {
                    if telemetry.endurance_percent.is_some() {
                        crate::locale::tr("aguardando histórico", "awaiting history")
                    } else {
                        crate::locale::tr("indisponível", "unavailable")
                    }
                    .into()
                }),
            lifetime_known: ready.remaining_seconds.is_some(),
            status: telemetry_status(telemetry),
            status_tone: status_tone(telemetry),
            wear: telemetry
                .endurance_percent
                .map(|wear| format!("{wear:.0}%"))
                .unwrap_or_else(|| crate::locale::tr("indisponível", "unavailable").into()),
            temperature: telemetry
                .temperature_c
                .map(|value| format!("{value} °C"))
                .unwrap_or_else(|| crate::locale::tr("indisponível", "unavailable").into()),
            power_on: telemetry
                .power_on_hours
                .map(|value| format!("{} h", compact_number(value)))
                .unwrap_or_else(|| crate::locale::tr("indisponível", "unavailable").into()),
            last_reading: ready
                .last_sample_unix
                .map(|timestamp| {
                    let elapsed = format_duration(now.saturating_sub(timestamp));
                    if crate::locale::english() {
                        format!("{elapsed} ago")
                    } else {
                        format!("há {elapsed}")
                    }
                })
                .unwrap_or_else(|| crate::locale::tr("indisponível", "unavailable").into()),
            warnings,
            details,
            attributes: match &telemetry.ata {
                QueryState::Success(ata) => ata.attributes.iter().map(attribute_row).collect(),
                _ => Vec::new(),
            },
        }
    }
}

fn attribute_row(attribute: &AtaAttribute) -> AttributeRow {
    // The six raw bytes are little-endian and vendor-defined: shown, never interpreted.
    let raw = attribute
        .raw
        .iter()
        .rev()
        .map(|byte| format!("{byte:02X}"))
        .collect();
    AttributeRow {
        id: format!("{:02X}", attribute.id),
        current: attribute.current.to_string(),
        worst: attribute.worst.to_string(),
        raw,
    }
}

const PERMISSION_MESSAGE: &str =
    "Permissão insuficiente para consultar o dispositivo. Algumas leituras estão indisponíveis.";

pub fn query_error_message(state: QueryErrorState) -> String {
    match state {
        QueryErrorState::Permission => crate::locale::tr(
            PERMISSION_MESSAGE,
            "Permission denied for this drive. Some readings are unavailable.",
        )
        .into(),
        QueryErrorState::Unsupported => crate::locale::tr(
            "Leitura de saúde não compatível com o dispositivo",
            "Drive health reporting is unsupported",
        )
        .into(),
        QueryErrorState::Transient | QueryErrorState::Partial => retry_message(DISCOVERY_INTERVAL),
    }
}

/// The retry is the next discovery, so the promised wait is that interval.
fn retry_message(interval: Duration) -> String {
    if crate::locale::english() {
        return format!(
            "Could not read drive health data. Retrying within {} seconds.",
            interval.as_secs()
        );
    }
    format!(
        "Não foi possível consultar os dados de saúde. Uma nova tentativa acontece em até {} segundos.",
        interval.as_secs()
    )
}

fn telemetry_has_usable_data(telemetry: &Telemetry) -> bool {
    matches!(&telemetry.nvme, QueryState::Success(_))
        || matches!(&telemetry.ata, QueryState::Success(ata) if !ata.attributes.is_empty())
        || matches!(&telemetry.overall_failure, QueryState::Success(_))
        || telemetry.endurance_percent.is_some()
        || telemetry.power_on_hours.is_some()
        || telemetry.temperature_c.is_some()
}

fn telemetry_status(telemetry: &Telemetry) -> &'static str {
    if !telemetry_has_usable_data(telemetry) {
        if any_permission_failure(telemetry) {
            return crate::locale::tr(
                PERMISSION_MESSAGE,
                "Permission denied for this drive. Some readings are unavailable.",
            );
        }
        if matches!(&telemetry.nvme, QueryState::Unsupported)
            || matches!(&telemetry.ata, QueryState::Unsupported)
            || matches!(&telemetry.overall_failure, QueryState::Unsupported)
        {
            return crate::locale::tr(
                "Leitura SMART não compatível",
                "SMART reporting is unsupported",
            );
        }
        if matches!(&telemetry.nvme, QueryState::Failed { .. })
            || matches!(&telemetry.ata, QueryState::Failed { .. })
            || matches!(&telemetry.overall_failure, QueryState::Failed { .. })
        {
            return crate::locale::tr("Falha ao ler dados SMART", "Could not read SMART data");
        }
        return crate::locale::tr(
            "Dados de saúde indisponíveis",
            "Drive health data unavailable",
        );
    }
    if any_permission_failure(telemetry) {
        return crate::locale::tr(
            "Dados parciais; permissão insuficiente em algumas consultas",
            "Partial data. Some queries require additional permissions.",
        );
    }
    if matches!(&telemetry.overall_failure, QueryState::Success(true)) {
        return crate::locale::tr(
            "O dispositivo relatou uma falha",
            "The drive reported a failure",
        );
    }
    if matches!(&telemetry.overall_failure, QueryState::Success(false))
        && !matches!(&telemetry.nvme, QueryState::Unsupported)
        && !matches!(&telemetry.ata, QueryState::Unsupported)
    {
        return crate::locale::tr(
            "Sem falha geral relatada pelo dispositivo",
            "The drive reported no overall failure",
        );
    }
    if matches!(
        &telemetry.nvme,
        QueryState::Unsupported | QueryState::Failed { .. }
    ) || matches!(
        &telemetry.ata,
        QueryState::Unsupported | QueryState::Failed { .. }
    ) || matches!(&telemetry.overall_failure, QueryState::Failed { .. })
    {
        return crate::locale::tr(
            "Dados de saúde parcialmente disponíveis",
            "Some drive health data is unavailable",
        );
    }
    crate::locale::tr("Dados SMART disponíveis", "SMART data available")
}

/// A device-reported failure is as loud as the pill; a permission gap only warns.
fn status_tone(telemetry: &Telemetry) -> Option<Tone> {
    if any_permission_failure(telemetry) {
        Some(Tone::Unknown)
    } else if matches!(&telemetry.overall_failure, QueryState::Success(true)) {
        Some(Tone::Critical)
    } else {
        None
    }
}

pub(crate) fn any_permission_failure(telemetry: &Telemetry) -> bool {
    failed_permission(&telemetry.nvme)
        || failed_permission(&telemetry.ata)
        || failed_permission(&telemetry.overall_failure)
}

fn failed_permission<T>(state: &QueryState<T>) -> bool {
    match state {
        QueryState::Failed { message } => is_permission_error(message),
        QueryState::NotQueried | QueryState::Unsupported | QueryState::Success(_) => false,
    }
}

fn is_permission_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("access is denied")
        || message.contains("acesso negado")
        || message.contains("error 5")
        || message == "5"
}

fn compact_number(value: u64) -> String {
    if value >= 1_000_000_000 {
        format!(
            "{} {}",
            decimal_pt(value as f64 / 1_000_000_000.0),
            crate::locale::tr("bi", "B")
        )
    } else if value >= 1_000_000 {
        format!(
            "{} {}",
            decimal_pt(value as f64 / 1_000_000.0),
            crate::locale::tr("mi", "M")
        )
    } else {
        value.to_string()
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{} {}", decimal_pt(size), UNITS[unit])
    }
}

fn decimal_pt(value: f64) -> String {
    format!("{value:.1}").replace('.', crate::locale::tr(",", "."))
}

fn format_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    if days >= 365 {
        let years = days / 365;
        if years == 1 {
            crate::locale::tr("1 ano", "1 year").into()
        } else {
            format!("{years} {}", crate::locale::tr("anos", "years"))
        }
    } else if days >= 30 {
        let months = days / 30;
        if months == 1 {
            crate::locale::tr("1 mês", "1 month").into()
        } else {
            format!("{months} {}", crate::locale::tr("meses", "months"))
        }
    } else if days == 1 {
        crate::locale::tr("1 dia", "1 day").into()
    } else if days > 0 {
        format!("{days} {}", crate::locale::tr("dias", "days"))
    } else {
        crate::locale::tr("menos de 1 dia", "less than one day").into()
    }
}

fn format_endurance_days(remaining_seconds: u64) -> String {
    let days = remaining_seconds / 86_400;
    if days == 1 {
        crate::locale::tr("1 dia", "1 day").into()
    } else {
        format!("{days} {}", crate::locale::tr("dias", "days"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        health::score_health,
        model::{AtaHealth, DiskIdentity, NvmeHealth},
    };

    fn identity(id: &str) -> DiskIdentity {
        DiskIdentity {
            id: id.into(),
            index: 0,
            model: None,
            serial: None,
            firmware: None,
            capacity_bytes: None,
            bus: None,
            kind: DiskKind::Unknown,
        }
    }

    fn card(identity: DiskIdentity, reading: Reading) -> Card {
        Card::new(&DiskSnapshot { identity, reading }, 1_000_000)
    }

    fn ready(telemetry: Telemetry) -> Reading {
        Reading::Ready(Box::new(Ready {
            telemetry,
            remaining_seconds: None,
            last_sample_unix: None,
        }))
    }

    fn attribute() -> AtaAttribute {
        AtaAttribute {
            id: 9,
            flags: 0,
            current: 100,
            worst: 100,
            raw: [0; 6],
        }
    }

    #[test]
    fn ui_generated_messages_are_brazilian_portuguese() {
        assert_eq!(format_duration(86_400), "1 dia");
        assert_eq!(format_duration(366 * 86_400), "1 ano");
        assert_eq!(format_endurance_days(0), "0 dias");
        assert_eq!(format_endurance_days(86_400), "1 dia");
        assert_eq!(format_endurance_days(3 * 86_400), "3 dias");
        assert_eq!(compact_number(1_234_567), "1,2 mi");

        let bare = card(identity("id"), Reading::Failed(QueryErrorState::Transient));
        assert_eq!(bare.title, "Modelo desconhecido");
        assert_eq!(bare.list_name, "Disco");
        assert_eq!(
            bare.subtitle,
            "Tipo desconhecido  ·  capacidade indisponível"
        );
        assert_eq!(
            bare.body,
            Body::Message(query_error_message(QueryErrorState::Transient))
        );

        let unknown = card(identity("id"), ready(Telemetry::default()));
        let Body::Health(health) = unknown.body else {
            panic!("a reading must produce a health body");
        };
        assert_eq!(health.label, "Indisponível");
        assert_eq!(health.tone, Tone::Unknown);
        assert_eq!(health.score, None);
        assert!(!health.lifetime_known);
        assert_eq!(health.status, "Dados de saúde indisponíveis");
        assert_eq!(health.lifetime, "indisponível");
        assert_eq!(health.wear, "indisponível");
        assert_eq!(health.last_reading, "indisponível");
    }

    #[test]
    fn status_labels_follow_the_health_outcome() {
        let healthy = Telemetry {
            nvme: QueryState::Success(NvmeHealth {
                percentage_used: Some(12),
                ..Default::default()
            }),
            endurance_percent: Some(12.0),
            ..Default::default()
        };
        let worn = Telemetry {
            nvme: QueryState::Success(NvmeHealth {
                percentage_used: Some(100),
                critical_warning: 1,
                ..Default::default()
            }),
            endurance_percent: Some(100.0),
            ..Default::default()
        };
        for (telemetry, label, tone) in [
            (healthy, "Saudável", Tone::Healthy),
            (worn, "Atenção", Tone::Attention),
            (Telemetry::default(), "Indisponível", Tone::Unknown),
        ] {
            let expected = telemetry
                .endurance_percent
                .map(|used| format!("{:.0}%", 100.0 - used));
            let mut disk = identity("nvme");
            disk.kind = DiskKind::Nvme;
            let card = card(disk, ready(telemetry));
            assert_eq!(card.tone(), Some(tone));
            let Body::Health(health) = card.body else {
                panic!("a reading must produce a health body");
            };
            assert_eq!((health.label, health.tone), (label, tone));
            if tone != Tone::Unknown {
                assert_eq!(health.score, expected);
            } else {
                assert_eq!(health.score, None);
            }
        }
    }

    #[test]
    fn query_failures_and_pending_disks_map_to_messages() {
        let disk = identity("a");
        assert_eq!(card(disk.clone(), Reading::Pending).body, Body::Loading);
        assert_eq!(card(disk.clone(), Reading::Pending).tone(), None);
        for (state, message) in [
            (QueryErrorState::Permission, PERMISSION_MESSAGE),
            (
                QueryErrorState::Unsupported,
                "Leitura de saúde não compatível com o dispositivo",
            ),
            (
                QueryErrorState::Transient,
                "Não foi possível consultar os dados de saúde. Uma nova tentativa acontece em até 15 segundos.",
            ),
            (
                QueryErrorState::Partial,
                "Não foi possível consultar os dados de saúde. Uma nova tentativa acontece em até 15 segundos.",
            ),
        ] {
            let failed = card(disk.clone(), Reading::Failed(state));
            assert_eq!(failed.body, Body::Message(message.into()));
            assert_eq!(failed.tone(), Some(Tone::Unknown));
        }
    }

    #[test]
    fn the_retry_message_states_the_discovery_interval() {
        assert!(retry_message(Duration::from_secs(40)).contains("até 40 segundos"));
        assert_eq!(
            query_error_message(QueryErrorState::Transient),
            retry_message(DISCOVERY_INTERVAL)
        );
    }

    #[test]
    fn partial_telemetry_is_not_mislabeled_as_unsupported() {
        let telemetry = Telemetry {
            nvme: QueryState::Unsupported,
            ata: QueryState::Success(AtaHealth {
                attributes: vec![attribute()],
            }),
            ..Default::default()
        };
        assert!(telemetry_has_usable_data(&telemetry));
        assert_ne!(telemetry_status(&telemetry), "Leitura SMART não compatível");
        assert_eq!(
            telemetry_status(&telemetry),
            "Dados de saúde parcialmente disponíveis"
        );

        let no_data = Telemetry {
            nvme: QueryState::Unsupported,
            ata: QueryState::Unsupported,
            ..Default::default()
        };
        assert!(!telemetry_has_usable_data(&no_data));
        assert_eq!(telemetry_status(&no_data), "Leitura SMART não compatível");

        let empty_success = Telemetry {
            nvme: QueryState::Unsupported,
            ata: QueryState::Success(AtaHealth::default()),
            ..Default::default()
        };
        assert!(!telemetry_has_usable_data(&empty_success));
        assert_eq!(
            telemetry_status(&empty_success),
            "Leitura SMART não compatível"
        );
    }

    #[test]
    fn partial_permission_failure_keeps_successful_score_and_warns_about_probe() {
        let telemetry = Telemetry {
            nvme: QueryState::Failed {
                message: "Access is denied (5)".into(),
            },
            ata: QueryState::Success(AtaHealth {
                attributes: vec![attribute()],
            }),
            endurance_percent: Some(12.0),
            ..Default::default()
        };

        assert_eq!(
            score_health(DiskKind::Ssd, &telemetry).outcome,
            HealthOutcome::Pass
        );
        assert_eq!(
            telemetry_status(&telemetry),
            "Dados parciais; permissão insuficiente em algumas consultas"
        );
        let Body::Health(health) = card(identity("ssd"), ready(telemetry)).body else {
            panic!("a reading must produce a health body");
        };
        assert_eq!(health.label, "Saudável");
        assert_eq!(health.status_tone, Some(Tone::Unknown));

        let no_data = Telemetry {
            nvme: QueryState::Failed {
                message: "Access is denied".into(),
            },
            ..Default::default()
        };
        assert_eq!(telemetry_status(&no_data), PERMISSION_MESSAGE);
        let Body::Health(health) = card(identity("ssd"), ready(no_data)).body else {
            panic!("a reading must produce a health body");
        };
        assert_eq!(health.status_tone, Some(Tone::Unknown));
    }

    #[test]
    fn a_device_reported_failure_speaks_as_loud_as_the_pill() {
        let failing = Telemetry {
            overall_failure: QueryState::Success(true),
            temperature_c: Some(30),
            ..Default::default()
        };
        let Body::Health(health) = card(identity("hdd"), ready(failing)).body else {
            panic!("a reading must produce a health body");
        };
        assert_eq!(health.status, "O dispositivo relatou uma falha");
        assert_eq!(health.status_tone, Some(Tone::Critical));

        let calm = Telemetry {
            overall_failure: QueryState::Success(false),
            temperature_c: Some(30),
            ..Default::default()
        };
        let Body::Health(health) = card(identity("hdd"), ready(calm)).body else {
            panic!("a reading must produce a health body");
        };
        assert_eq!(health.status_tone, None);
    }

    #[test]
    fn lifetime_last_reading_warnings_and_attributes_are_formatted() {
        let telemetry = Telemetry {
            nvme: QueryState::Success(NvmeHealth {
                critical_warning: 2 | 8,
                ..Default::default()
            }),
            ata: QueryState::Success(AtaHealth {
                attributes: vec![AtaAttribute {
                    id: 0xC2,
                    flags: 0,
                    current: 71,
                    worst: 53,
                    raw: [0x22, 0x01, 0, 0, 0, 0],
                }],
            }),
            endurance_percent: Some(7.0),
            power_on_hours: Some(4_200_000),
            temperature_c: Some(38),
            ..Default::default()
        };
        let snapshot = DiskSnapshot {
            identity: identity("a"),
            reading: Reading::Ready(Box::new(Ready {
                telemetry,
                remaining_seconds: Some(412 * 86_400 + 5),
                last_sample_unix: Some(1_000_000 - 3 * 86_400),
            })),
        };
        let Body::Health(health) = Card::new(&snapshot, 1_000_000).body else {
            panic!("a reading must produce a health body");
        };
        assert_eq!(health.lifetime, "412 dias");
        assert!(health.lifetime_known);
        assert_eq!(health.last_reading, "há 3 dias");
        assert_eq!(health.wear, "7%");
        assert_eq!(health.temperature, "38 °C");
        assert_eq!(health.power_on, "4,2 mi h");
        assert_eq!(
            health.warnings,
            ["Temperatura NVMe crítica", "Mídia NVMe somente leitura"]
        );
        assert_eq!(
            health.attributes,
            [AttributeRow {
                id: "C2".into(),
                current: "71".into(),
                worst: "53".into(),
                raw: "000000000122".into(),
            }]
        );
    }
}
