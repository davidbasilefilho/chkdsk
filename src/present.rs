//! Texto em português brasileiro. A tradução fica nesta fronteira: detalhes e
//! mensagens do sistema operacional nunca são exibidos diretamente. Os cartões
//! são montados quando um resultado chega, para que a janela só desenhe.

use crate::{
    backend::QueryErrorState,
    collector::{DiskSnapshot, Reading, Ready},
    health::{HealthOutcome, score_health},
    model::{AtaAttribute, DiskKind, QueryState, Telemetry},
};

/// The three colors of the palette: healthy, attention, no usable data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Healthy,
    Attention,
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
    /// `93`, or `N/A` when there is no usable evidence.
    pub score: String,
    pub scored: bool,
    /// `412 dias` or `aguardando histórico`.
    pub lifetime: String,
    pub status: &'static str,
    /// The status line is about missing permission.
    pub status_alert: bool,
    pub wear: String,
    pub temperature: String,
    pub power_on: String,
    pub last_reading: String,
    pub warnings: Vec<&'static str>,
    pub attributes: Vec<AttributeRow>,
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
            DiskKind::Hdd => "HD",
            DiskKind::Ssd => "SSD",
            DiskKind::Nvme => "NVMe",
            DiskKind::Unknown => "Tipo desconhecido",
        };
        let capacity = identity
            .capacity_bytes
            .map(format_bytes)
            .unwrap_or_else(|| "capacidade indisponível".into());
        let body = match &snapshot.reading {
            Reading::Pending => Body::Loading,
            Reading::Failed(state) => Body::Message(query_error_message(*state).into()),
            Reading::Ready(ready) => Body::Health(Box::new(Health::new(identity.kind, ready, now))),
        };
        Card {
            id: identity.id.clone(),
            list_name: model.unwrap_or("Disco").into(),
            title: model.unwrap_or("Modelo desconhecido").into(),
            subtitle: format!("{kind}  ·  {capacity}"),
            body,
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
    fn new(kind: DiskKind, ready: &Ready, now: u64) -> Self {
        let telemetry = &ready.telemetry;
        let health = score_health(kind, telemetry);
        let (label, tone) = match health.outcome {
            HealthOutcome::Pass => ("SAUDÁVEL", Tone::Healthy),
            HealthOutcome::Fail => ("ATENÇÃO", Tone::Attention),
            HealthOutcome::Unknown => ("INDISPONÍVEL", Tone::Unknown),
        };
        let scored = health.outcome != HealthOutcome::Unknown;
        let warnings = match &telemetry.nvme {
            QueryState::Success(nvme) => [
                (1, "Reserva NVMe crítica"),
                (2, "Temperatura NVMe crítica"),
                (4, "Confiabilidade NVMe degradada"),
                (8, "Mídia NVMe somente leitura"),
                (16, "Aviso de memória NVMe"),
            ]
            .into_iter()
            .filter(|(bit, _)| nvme.critical_warning & bit != 0)
            .map(|(_, warning)| warning)
            .collect(),
            _ => Vec::new(),
        };
        Health {
            tone,
            label,
            score: if scored {
                health.score.to_string()
            } else {
                "N/A".into()
            },
            scored,
            lifetime: ready
                .remaining_seconds
                .map(format_endurance_days)
                .unwrap_or_else(|| "aguardando histórico".into()),
            status: telemetry_status(telemetry),
            status_alert: any_permission_failure(telemetry),
            wear: telemetry
                .endurance_percent
                .map(|wear| format!("{wear:.0}%"))
                .unwrap_or_else(|| "indisponível".into()),
            temperature: telemetry
                .temperature_c
                .map(|value| format!("{value} °C"))
                .unwrap_or_else(|| "indisponível".into()),
            power_on: telemetry
                .power_on_hours
                .map(|value| format!("{} h", compact_number(value)))
                .unwrap_or_else(|| "indisponível".into()),
            last_reading: ready
                .last_sample_unix
                .map(|timestamp| format!("há {}", format_duration(now.saturating_sub(timestamp))))
                .unwrap_or_else(|| "indisponível".into()),
            warnings,
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

pub fn query_error_message(state: QueryErrorState) -> &'static str {
    match state {
        QueryErrorState::Permission => "Permissão insuficiente para consultar o dispositivo",
        QueryErrorState::Unsupported => "Leitura de saúde não compatível com o dispositivo",
        QueryErrorState::Transient | QueryErrorState::Partial => {
            "Não foi possível consultar os dados de saúde"
        }
    }
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
            return "Permissão insuficiente para consultar o dispositivo";
        }
        if matches!(&telemetry.nvme, QueryState::Unsupported)
            || matches!(&telemetry.ata, QueryState::Unsupported)
            || matches!(&telemetry.overall_failure, QueryState::Unsupported)
        {
            return "Leitura SMART não compatível";
        }
        if matches!(&telemetry.nvme, QueryState::Failed { .. })
            || matches!(&telemetry.ata, QueryState::Failed { .. })
            || matches!(&telemetry.overall_failure, QueryState::Failed { .. })
        {
            return "Falha ao ler dados SMART";
        }
        return "Dados de saúde indisponíveis";
    }
    if any_permission_failure(telemetry) {
        return "Dados parciais; permissão insuficiente em algumas consultas";
    }
    if matches!(&telemetry.overall_failure, QueryState::Success(true)) {
        return "O dispositivo relatou uma falha";
    }
    if matches!(&telemetry.overall_failure, QueryState::Success(false))
        && !matches!(&telemetry.nvme, QueryState::Unsupported)
        && !matches!(&telemetry.ata, QueryState::Unsupported)
    {
        return "Sem falha geral relatada pelo dispositivo";
    }
    if matches!(
        &telemetry.nvme,
        QueryState::Unsupported | QueryState::Failed { .. }
    ) || matches!(
        &telemetry.ata,
        QueryState::Unsupported | QueryState::Failed { .. }
    ) || matches!(&telemetry.overall_failure, QueryState::Failed { .. })
    {
        return "Dados de saúde parcialmente disponíveis";
    }
    "Dados SMART disponíveis"
}

fn any_permission_failure(telemetry: &Telemetry) -> bool {
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
        format!("{} bi", decimal_pt(value as f64 / 1_000_000_000.0))
    } else if value >= 1_000_000 {
        format!("{} mi", decimal_pt(value as f64 / 1_000_000.0))
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
    format!("{value:.1}").replace('.', ",")
}

fn format_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    if days >= 365 {
        let years = days / 365;
        if years == 1 {
            "1 ano".into()
        } else {
            format!("{years} anos")
        }
    } else if days >= 30 {
        let months = days / 30;
        if months == 1 {
            "1 mês".into()
        } else {
            format!("{months} meses")
        }
    } else if days == 1 {
        "1 dia".into()
    } else if days > 0 {
        format!("{days} dias")
    } else {
        "menos de 1 dia".into()
    }
}

fn format_endurance_days(remaining_seconds: u64) -> String {
    let days = remaining_seconds / 86_400;
    if days == 1 {
        "1 dia".into()
    } else {
        format!("{days} dias")
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
            Body::Message("Não foi possível consultar os dados de saúde".into())
        );

        let unknown = card(identity("id"), ready(Telemetry::default()));
        let Body::Health(health) = unknown.body else {
            panic!("a reading must produce a health body");
        };
        assert_eq!(health.label, "INDISPONÍVEL");
        assert_eq!(health.tone, Tone::Unknown);
        assert_eq!(health.score, "N/A");
        assert!(!health.scored);
        assert_eq!(health.status, "Dados de saúde indisponíveis");
        assert_eq!(health.lifetime, "aguardando histórico");
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
            (healthy, "SAUDÁVEL", Tone::Healthy),
            (worn, "ATENÇÃO", Tone::Attention),
            (Telemetry::default(), "INDISPONÍVEL", Tone::Unknown),
        ] {
            let expected = score_health(DiskKind::Nvme, &telemetry);
            let mut disk = identity("nvme");
            disk.kind = DiskKind::Nvme;
            let card = card(disk, ready(telemetry));
            assert_eq!(card.tone(), Some(tone));
            let Body::Health(health) = card.body else {
                panic!("a reading must produce a health body");
            };
            assert_eq!((health.label, health.tone), (label, tone));
            if health.scored {
                assert_eq!(health.score, expected.score.to_string());
            }
        }
    }

    #[test]
    fn query_failures_and_pending_disks_map_to_messages() {
        let disk = identity("a");
        assert_eq!(card(disk.clone(), Reading::Pending).body, Body::Loading);
        assert_eq!(card(disk.clone(), Reading::Pending).tone(), None);
        for (state, message) in [
            (
                QueryErrorState::Permission,
                "Permissão insuficiente para consultar o dispositivo",
            ),
            (
                QueryErrorState::Unsupported,
                "Leitura de saúde não compatível com o dispositivo",
            ),
            (
                QueryErrorState::Transient,
                "Não foi possível consultar os dados de saúde",
            ),
            (
                QueryErrorState::Partial,
                "Não foi possível consultar os dados de saúde",
            ),
        ] {
            let failed = card(disk.clone(), Reading::Failed(state));
            assert_eq!(failed.body, Body::Message(message.into()));
            assert_eq!(failed.tone(), Some(Tone::Unknown));
        }
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
        assert_eq!(health.label, "SAUDÁVEL");
        assert!(health.status_alert);

        let no_data = Telemetry {
            nvme: QueryState::Failed {
                message: "Access is denied".into(),
            },
            ..Default::default()
        };
        assert_eq!(
            telemetry_status(&no_data),
            "Permissão insuficiente para consultar o dispositivo"
        );
        let Body::Health(health) = card(identity("ssd"), ready(no_data)).body else {
            panic!("a reading must produce a health body");
        };
        assert!(health.status_alert);
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
