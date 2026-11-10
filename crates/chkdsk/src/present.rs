//! Texto em português brasileiro. A tradução fica nesta fronteira: detalhes e
//! mensagens do sistema operacional nunca são exibidos diretamente. Os cartões
//! são montados quando um resultado chega, para que a janela só desenhe.

use crate::collector::{DISCOVERY_INTERVAL, DiskSnapshot, Reading, Ready};
use chkdsk_core::{
    health::{HealthOutcome, HealthScore, score_health},
    model::{
        AtaAttribute, AtaHealth, DiskIdentity, DiskKind, HealthWarningReason, QueryState, Telemetry,
    },
};
use chkdsk_windows::backend::QueryErrorState;
use std::{collections::HashSet, time::Duration};

/// How loudly a disk speaks: healthy, a passing disk with warnings (caution),
/// a device-reported failure (attention) or no usable data. Caution reads "Atenção"
/// and attention reads "Crítico", so the two differ in words as well as color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Healthy,
    Caution,
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
    /// Bus, firmware, serial and data sources, for the collapsed Avançado section.
    pub details: Vec<Detail>,
    pub body: Body,
}

/// One technical fact about a disk: `Barramento`, `NVMe`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detail {
    pub label: &'static str,
    pub value: String,
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
    /// 0 to 100; none when there is no usable evidence.
    pub score: Option<u8>,
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
    /// Device-reported failures and softer signals behind the score, one line each.
    pub warnings: Vec<String>,
    /// What to do about an Atenção or Crítico verdict; none otherwise.
    pub advice: Option<&'static str>,
    pub attributes: Vec<AttributeRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributeRow {
    pub id: String,
    /// The attribute's usual name where it is the same on every drive.
    pub name: String,
    pub current: String,
    pub worst: String,
    /// The device's declared failure threshold; `—` when it declares none.
    pub threshold: String,
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
        // Discovery may not know the kind; a successful reading can tell.
        let kind = match match &snapshot.reading {
            Reading::Ready(ready) => ready.telemetry.effective_kind(identity),
            Reading::Pending | Reading::Failed(_) => identity.kind,
        } {
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
            Reading::Failed(state) => Body::Message(query_error_message(*state)),
            Reading::Ready(ready) => Body::Health(Box::new(Health::new(identity, ready, now))),
        };
        Card {
            id: identity.id.clone(),
            list_name: model.unwrap_or("Disco").into(),
            title: model.unwrap_or("Modelo desconhecido").into(),
            subtitle: format!("{kind}  ·  {capacity}"),
            details: details(identity, &snapshot.reading),
            body,
        }
    }

    /// Avançado has something to show: technical details or the ATA table.
    pub fn has_advanced(&self) -> bool {
        let attributes = match &self.body {
            Body::Health(health) => health.attributes.len(),
            _ => 0,
        };
        !self.details.is_empty() || attributes > 0
    }

    /// The word next to the disk in the list, so color is never the only signal.
    pub fn status_label(&self) -> &'static str {
        match &self.body {
            Body::Loading => "Lendo…",
            Body::Message(_) => "Indisponível",
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
    fn new(identity: &DiskIdentity, ready: &Ready, now: u64) -> Self {
        let telemetry = &ready.telemetry;
        let health = score_health(telemetry.effective_kind(identity), telemetry);
        // Soft signals keep a passing disk from reading as plainly healthy.
        let (label, tone) = match health.outcome {
            HealthOutcome::Pass if health.warnings.is_empty() && health.score > 50 => {
                ("Saudável", Tone::Healthy)
            }
            HealthOutcome::Pass => ("Atenção", Tone::Caution),
            HealthOutcome::Fail => ("Crítico", Tone::Attention),
            HealthOutcome::Unknown => ("Indisponível", Tone::Unknown),
        };
        let score = (health.outcome != HealthOutcome::Unknown).then_some(health.score);
        let status = match telemetry_status(telemetry) {
            SMART_AVAILABLE if health.outcome == HealthOutcome::Unknown => SMART_INSUFFICIENT,
            status => status,
        };
        Health {
            tone,
            label,
            score,
            lifetime: ready
                .remaining_seconds
                .map(format_endurance_days)
                .unwrap_or_else(|| "aguardando histórico".into()),
            lifetime_known: ready.remaining_seconds.is_some(),
            status,
            status_tone: status_tone(telemetry),
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
            warnings: warning_lines(&health, telemetry),
            advice: matches!(tone, Tone::Caution | Tone::Attention).then_some(BACKUP_ADVICE),
            attributes: match &telemetry.ata {
                QueryState::Success(ata) => ata.attributes.iter().map(attribute_row).collect(),
                _ => Vec::new(),
            },
        }
    }
}

const BACKUP_ADVICE: &str = "Faça backup dos dados importantes.";

/// Failures the device declared, then the softer signals behind the score.
fn warning_lines(health: &HealthScore, telemetry: &Telemetry) -> Vec<String> {
    let mut lines = Vec::new();
    if let QueryState::Success(nvme) = &telemetry.nvme {
        let spare_below_threshold = matches!(
            (nvme.available_spare_percent, nvme.available_spare_threshold_percent),
            (Some(spare), Some(threshold)) if spare < threshold
        );
        if spare_below_threshold
            && nvme.critical_warning & 1 == 0
            && let (Some(spare), Some(threshold)) = (
                nvme.available_spare_percent,
                nvme.available_spare_threshold_percent,
            )
        {
            lines.push(format!(
                "Reserva NVMe abaixo do limite ({spare}% < {threshold}%)"
            ));
        }
        // These bits also appear as score warnings; they are worded here once.
        for (bit, warning) in [
            (1, "Reserva NVMe crítica"),
            (2, "Temperatura NVMe crítica"),
            (4, "Confiabilidade NVMe degradada"),
            (8, "Mídia NVMe somente leitura"),
            (16, "Aviso de memória NVMe"),
            (32, "Região de memória persistente NVMe degradada"),
        ] {
            if nvme.critical_warning & bit != 0 {
                lines.push(warning.into());
            }
        }
    }
    if let QueryState::Success(ata) = &telemetry.ata {
        for attribute in &ata.attributes {
            if let Some(threshold) = ata_threshold(ata, attribute)
                && attribute.current <= threshold
                && attribute.flags & 1 != 0
            {
                lines.push(format!(
                    "Falha iminente, limite atingido: {}, {}",
                    attribute_label(attribute.id),
                    values_against(attribute.current, threshold)
                ));
            }
        }
    }
    for reason in &health.warnings {
        match reason {
            HealthWarningReason::AtaThresholdMargin => lines.extend(
                ata_threshold_lines(telemetry, true)
                    .unwrap_or_else(|| vec!["SMART ATA próximo do limite declarado".into()]),
            ),
            HealthWarningReason::AtaThresholdReached => lines.extend(
                ata_threshold_lines(telemetry, false)
                    .unwrap_or_else(|| vec!["SMART ATA atingiu o limite declarado".into()]),
            ),
            HealthWarningReason::EnduranceExhausted => {
                lines.push("Vida útil nominal do dispositivo esgotada".into());
            }
            HealthWarningReason::AtaReallocatedSectors => {
                lines.push("Setores realocados registrados na mídia".into());
            }
            HealthWarningReason::AtaSpinRetries => {
                lines.push("Tentativas repetidas de giro registradas".into());
            }
            HealthWarningReason::AtaReportedUncorrectable => {
                lines.push("Erros incorrigíveis reportados pelo dispositivo".into());
            }
            HealthWarningReason::AtaPendingSectors => {
                lines.push("Setores pendentes aguardando remapeamento".into());
            }
            HealthWarningReason::AtaOfflineUncorrectable => {
                lines.push("Setores incorrigíveis encontrados em verificações offline".into());
            }
            HealthWarningReason::AtaRawEncodingAmbiguous => lines
                .push("Codificação bruta SMART ambígua; nota de saúde do HD indisponível".into()),
            HealthWarningReason::NvmeMediaErrors => lines.push(match &telemetry.nvme {
                QueryState::Success(nvme) => nvme
                    .media_errors
                    .filter(|count| *count > 0)
                    .map(|count| format!("Erros de mídia NVMe registrados: {count}"))
                    .unwrap_or_else(|| "Erros de mídia NVMe registrados".into()),
                _ => "Erros de mídia NVMe registrados".into(),
            }),
            // Worded with the critical-warning bits above.
            HealthWarningReason::NvmeTemperatureThreshold
            | HealthWarningReason::NvmeVolatileMemoryBackup
            | HealthWarningReason::NvmePersistentMemoryRegion => {}
        }
    }
    let mut seen = HashSet::new();
    lines.retain(|line| seen.insert(line.clone()));
    lines
}

/// The declared threshold of an attribute that can be judged: it appears once,
/// holds a valid normalized value and declares a nonzero threshold.
fn ata_threshold(ata: &AtaHealth, attribute: &AtaAttribute) -> Option<u8> {
    if chkdsk_core::ata::unique_attribute(ata, attribute.id).is_none()
        || !(1..=254).contains(&attribute.current)
    {
        return None;
    }
    attribute.threshold.filter(|threshold| *threshold > 0)
}

/// Attributes within ten points of their threshold (`near`) or at it, for the
/// soft warnings. Pre-failure attributes at the threshold are failures, worded above.
fn ata_threshold_lines(telemetry: &Telemetry, near: bool) -> Option<Vec<String>> {
    let QueryState::Success(ata) = &telemetry.ata else {
        return None;
    };
    let lines: Vec<String> = ata
        .attributes
        .iter()
        .filter(|attribute| attribute.flags & 1 == 0)
        .filter_map(|attribute| {
            let threshold = ata_threshold(ata, attribute)?;
            let matches = if near {
                attribute.current > threshold && attribute.current <= threshold.saturating_add(10)
            } else {
                attribute.current <= threshold
            };
            matches.then(|| {
                let description = if near {
                    "Próximo do limite"
                } else {
                    "Limite atingido"
                };
                format!(
                    "{description}: {}, {}",
                    attribute_label(attribute.id),
                    values_against(attribute.current, threshold)
                )
            })
        })
        .collect();
    (!lines.is_empty()).then_some(lines)
}

/// `valor 41, limite 36`: the normalized value next to the threshold it is judged by.
fn values_against(current: u8, threshold: u8) -> String {
    // No-break spaces keep a number with its word when the line wraps.
    format!("valor\u{a0}{current}, limite\u{a0}{threshold}")
}

/// `Setores realocados (05)`: the attribute's usual name where it is the same on
/// every drive, so the number in the table can be found; otherwise only its ID.
fn attribute_label(id: u8) -> String {
    format!(
        "{} ({id:02X})",
        attribute_name(id).unwrap_or("Atributo ATA")
    )
}

/// The name most drives agree on; vendors define the rest.
pub fn attribute_name(id: u8) -> Option<&'static str> {
    Some(match id {
        0x01 => "Taxa de erros de leitura",
        0x03 => "Tempo de aceleração do disco",
        0x04 => "Ciclos de partida e parada",
        0x05 => "Setores realocados",
        0x07 => "Taxa de erros de posicionamento",
        0x09 => "Horas ligado",
        0x0A => "Tentativas de giro",
        0x0C => "Ciclos de energia",
        0xBB => "Erros incorrigíveis reportados",
        0xC4 => "Eventos de realocação",
        0xC5 => "Setores pendentes",
        0xC6 => "Setores incorrigíveis offline",
        0xC7 => "Erros de CRC na interface",
        _ => return None,
    })
}

/// `96%`: the score as the window and the exports write it.
pub fn percent(value: u8) -> String {
    format!("{value}%")
}

/// What the Avançado section lists besides the attribute table: where the disk
/// sits on the machine and which readings the numbers above came from.
fn details(identity: &DiskIdentity, reading: &Reading) -> Vec<Detail> {
    let mut details = Vec::new();
    let mut add = |label: &'static str, value: Option<&str>| {
        if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
            details.push(Detail {
                label,
                value: value.to_owned(),
            });
        }
    };
    add("Barramento", identity.bus.as_deref());
    add("Firmware", identity.firmware.as_deref());
    add("Número de série", identity.serial.as_deref());
    if let Reading::Ready(ready) = reading {
        let sources = data_sources(&ready.telemetry);
        add(
            "Fontes dos dados",
            (!sources.is_empty()).then(|| sources.join(", ")).as_deref(),
        );
        add(
            "Perfil de atributos",
            ready.telemetry.ata_profile.as_deref(),
        );
    }
    details
}

/// The readings that succeeded, as the user would name them.
fn data_sources(telemetry: &Telemetry) -> Vec<&'static str> {
    let mut sources = Vec::new();
    if matches!(&telemetry.nvme, QueryState::Success(_)) {
        sources.push("log de saúde NVMe");
    }
    if let QueryState::Success(ata) = &telemetry.ata {
        sources.push(if ata.thresholds_available {
            "SMART ATA com limites"
        } else {
            "SMART ATA sem limites"
        });
    }
    if matches!(&telemetry.overall_failure, QueryState::Success(_)) {
        sources.push("estado geral SMART");
    }
    sources
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
        name: attribute_name(attribute.id)
            .unwrap_or("Específico do fabricante")
            .into(),
        current: attribute.current.to_string(),
        worst: attribute.worst.to_string(),
        threshold: attribute
            .threshold
            .map_or_else(|| "—".into(), |threshold| threshold.to_string()),
        raw,
    }
}

const PERMISSION_MESSAGE: &str =
    "Permissão insuficiente para consultar o dispositivo. Tente executar como administrador.";

pub fn query_error_message(state: QueryErrorState) -> String {
    match state {
        QueryErrorState::Permission => PERMISSION_MESSAGE.into(),
        QueryErrorState::Unsupported => "Leitura de saúde não compatível com o dispositivo".into(),
        QueryErrorState::Transient | QueryErrorState::Partial => retry_message(DISCOVERY_INTERVAL),
    }
}

/// The retry is the next discovery, so the promised wait is that interval.
fn retry_message(interval: Duration) -> String {
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

const SMART_AVAILABLE: &str = "Dados SMART disponíveis";
const SMART_INSUFFICIENT: &str = "Dados SMART insuficientes para calcular a nota";

fn telemetry_status(telemetry: &Telemetry) -> &'static str {
    if !telemetry_has_usable_data(telemetry) {
        if any_permission_failure(telemetry) {
            return PERMISSION_MESSAGE;
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
    SMART_AVAILABLE
}

/// A device-reported failure is as loud as the pill; a permission gap only warns.
fn status_tone(telemetry: &Telemetry) -> Option<Tone> {
    if any_permission_failure(telemetry) {
        Some(Tone::Unknown)
    } else if matches!(&telemetry.overall_failure, QueryState::Success(true)) {
        Some(Tone::Attention)
    } else {
        None
    }
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
    use chkdsk_core::{
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
            threshold: None,
            raw: [0; 6],
        }
    }

    fn ata(attributes: Vec<AtaAttribute>) -> QueryState<AtaHealth> {
        QueryState::Success(AtaHealth {
            attributes,
            thresholds_available: true,
        })
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
            (healthy, "Saudável", Tone::Healthy),
            (worn, "Crítico", Tone::Attention),
            (Telemetry::default(), "Indisponível", Tone::Unknown),
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
            if tone != Tone::Unknown {
                assert_eq!(health.score, Some(expected.score));
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
            ata: ata(vec![attribute()]),
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
            ata: ata(vec![attribute()]),
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
        assert_eq!(health.status_tone, Some(Tone::Attention));

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
            ata: ata(vec![AtaAttribute {
                id: 0xC2,
                flags: 0,
                current: 71,
                worst: 53,
                threshold: Some(10),
                raw: [0x22, 0x01, 0, 0, 0, 0],
            }]),
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
                name: "Específico do fabricante".into(),
                current: "71".into(),
                worst: "53".into(),
                threshold: "10".into(),
                raw: "000000000122".into(),
            }]
        );
    }

    fn health_of(telemetry: Telemetry, kind: DiskKind) -> Health {
        let mut disk = identity("disk");
        disk.kind = kind;
        let Body::Health(health) = card(disk, ready(telemetry)).body else {
            panic!("a reading must produce a health body");
        };
        *health
    }

    #[test]
    fn a_passing_disk_with_soft_signals_reads_as_caution_and_says_why() {
        let telemetry = Telemetry {
            nvme: QueryState::Success(NvmeHealth {
                percentage_used: Some(4),
                media_errors: Some(12),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            score_health(DiskKind::Nvme, &telemetry).outcome,
            HealthOutcome::Pass
        );
        let health = health_of(telemetry, DiskKind::Nvme);
        assert_eq!((health.label, health.tone), ("Atenção", Tone::Caution));
        assert!(health.score.is_some());
        assert_eq!(
            health.advice,
            Some(BACKUP_ADVICE),
            "caution still advises a backup"
        );
        assert_eq!(health.warnings, ["Erros de mídia NVMe registrados: 12"]);

        let exhausted = Telemetry {
            nvme: QueryState::Success(NvmeHealth {
                percentage_used: Some(100),
                ..Default::default()
            }),
            ..Default::default()
        };
        let health = health_of(exhausted, DiskKind::Nvme);
        assert_eq!(health.label, "Atenção");
        assert_eq!(
            health.warnings,
            ["Vida útil nominal do dispositivo esgotada"]
        );
    }

    #[test]
    fn nvme_critical_bits_are_worded_once_next_to_their_score_warnings() {
        let telemetry = Telemetry {
            nvme: QueryState::Success(NvmeHealth {
                critical_warning: 1 | 2 | 16 | 32,
                available_spare_percent: Some(5),
                available_spare_threshold_percent: Some(10),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            health_of(telemetry, DiskKind::Nvme).warnings,
            [
                "Reserva NVMe crítica",
                "Temperatura NVMe crítica",
                "Aviso de memória NVMe",
                "Região de memória persistente NVMe degradada",
            ]
        );

        // Without the critical bit, the spare level below its threshold is the failure.
        let spare_only = Telemetry {
            nvme: QueryState::Success(NvmeHealth {
                available_spare_percent: Some(5),
                available_spare_threshold_percent: Some(10),
                ..Default::default()
            }),
            ..Default::default()
        };
        let health = health_of(spare_only, DiskKind::Nvme);
        assert_eq!(health.label, "Crítico");
        assert_eq!(
            health.warnings,
            ["Reserva NVMe abaixo do limite (5% < 10%)"]
        );
    }

    #[test]
    fn ata_thresholds_name_the_attribute_and_fill_the_limit_column() {
        let row = |id, flags, current, threshold| AtaAttribute {
            id,
            flags,
            current,
            worst: current,
            threshold,
            raw: [0; 6],
        };
        let telemetry = Telemetry {
            ata: ata(vec![
                row(0x05, 1, 30, Some(36)),
                row(0xC4, 0, 41, Some(36)),
                row(0x09, 0, 60, None),
                // A duplicated ID is ambiguous and never judged.
                row(0x0A, 1, 1, Some(51)),
                row(0x0A, 1, 1, Some(51)),
            ]),
            ..Default::default()
        };
        let health = health_of(telemetry, DiskKind::Hdd);
        assert_eq!((health.label, health.tone), ("Crítico", Tone::Attention));
        assert_eq!(
            health.warnings,
            [
                "Falha iminente, limite atingido: Setores realocados (05), valor\u{a0}30, limite\u{a0}36",
                "Próximo do limite: Eventos de realocação (C4), valor\u{a0}41, limite\u{a0}36",
            ]
        );
        assert_eq!(
            health
                .attributes
                .iter()
                .map(|row| row.threshold.as_str())
                .collect::<Vec<_>>(),
            ["36", "36", "—", "51", "51"]
        );
    }

    #[test]
    fn hdd_counters_and_ambiguous_encodings_are_worded_in_portuguese() {
        let counter = |id, raw0, raw2| AtaAttribute {
            id,
            flags: 0,
            current: 100,
            worst: 100,
            threshold: None,
            raw: [raw0, 0, raw2, 0, 0, 0],
        };
        let reallocated = Telemetry {
            ata: ata(vec![counter(5, 3, 0), counter(197, 1, 0)]),
            ..Default::default()
        };
        assert_eq!(
            health_of(reallocated, DiskKind::Hdd).warnings,
            [
                "Setores realocados registrados na mídia",
                "Setores pendentes aguardando remapeamento",
            ]
        );

        // ID 5 with a nonzero upper word is not decoded: no grade, and it says so.
        let ambiguous = Telemetry {
            ata: ata(vec![counter(5, 3, 1)]),
            ..Default::default()
        };
        let health = health_of(ambiguous, DiskKind::Hdd);
        assert_eq!((health.label, health.score), ("Indisponível", None));
        assert_eq!(
            health.warnings,
            ["Codificação bruta SMART ambígua; nota de saúde do HD indisponível"]
        );
    }

    #[test]
    fn a_coarse_pass_alone_is_not_a_grade_and_says_so() {
        let health = health_of(
            Telemetry {
                overall_failure: QueryState::Success(false),
                ..Default::default()
            },
            DiskKind::Hdd,
        );
        assert_eq!((health.label, health.score), ("Indisponível", None));
        assert_eq!(health.status, "Sem falha geral relatada pelo dispositivo");

        let unscored = health_of(
            Telemetry {
                ata: ata(vec![attribute()]),
                ..Default::default()
            },
            DiskKind::Ssd,
        );
        assert_eq!((unscored.label, unscored.score), ("Indisponível", None));
        assert_eq!(
            unscored.status,
            "Dados SMART insuficientes para calcular a nota"
        );
    }

    #[test]
    fn a_successful_reading_names_the_kind_discovery_could_not() {
        let mut telemetry = Telemetry::default();
        telemetry.apply_nvme(NvmeHealth {
            percentage_used: Some(12),
            ..Default::default()
        });
        let shown = card(identity("unknown"), ready(telemetry.clone()));
        assert_eq!(shown.subtitle, "NVMe  ·  capacidade indisponível");
        let Body::Health(health) = shown.body else {
            panic!("a reading must produce a health body");
        };
        assert_eq!(health.label, "Saudável");

        let pending = card(identity("unknown"), Reading::Pending);
        assert_eq!(
            pending.subtitle,
            "Tipo desconhecido  ·  capacidade indisponível"
        );
    }
}
