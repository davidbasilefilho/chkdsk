//! Texto em português brasileiro. A tradução fica nesta fronteira: detalhes e
//! mensagens do sistema operacional nunca são exibidos diretamente. Os cartões
//! são montados quando um resultado chega, para que a janela só desenhe.

use crate::collector::{DISCOVERY_INTERVAL, DiskSnapshot, Reading, Ready};
use chkdsk_core::{
    ata::generic_decode,
    health::{HealthOutcome, HealthScore, score_health},
    model::{
        AtaAttribute, AtaHealth, DiskIdentity, DiskKind, HealthWarningReason, QueryState, Telemetry,
    },
};
use chkdsk_windows::backend::QueryErrorState;
use std::{collections::HashSet, time::Duration};

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
    /// Device-reported failures and softer signals behind the score, one line each.
    pub warnings: Vec<String>,
    /// What to do about an Atenção verdict or a limited assessment.
    pub advice: Option<&'static str>,
    /// Counters that SMART reports the same way on every drive, one row each.
    pub details: Vec<Detail>,
    pub attributes: Vec<AttributeRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detail {
    pub label: &'static str,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributeRow {
    pub id: String,
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
        let kind = match &snapshot.reading {
            Reading::Ready(ready) => ready.telemetry.effective_kind(identity),
            Reading::Pending | Reading::Failed(_) => identity.kind,
        };
        let usb = is_usb(identity);
        let kind_label = match kind {
            DiskKind::Hdd => "HD",
            DiskKind::Ssd => "SSD",
            DiskKind::Nvme => "NVMe",
            // A bridge often hides the media: the bus is still worth naming.
            DiskKind::Unknown if usb => "USB",
            DiskKind::Unknown => "Tipo desconhecido",
        };
        let capacity = identity
            .capacity_bytes
            .map(format_bytes)
            .unwrap_or_else(|| "capacidade indisponível".into());
        let subtitle = if usb && kind != DiskKind::Unknown {
            format!("{kind_label}  ·  USB  ·  {capacity}")
        } else {
            format!("{kind_label}  ·  {capacity}")
        };
        let body = match &snapshot.reading {
            Reading::Pending => Body::Loading,
            Reading::Failed(state) => Body::Message(query_error_message_for(identity, *state)),
            Reading::Ready(ready) => Body::Health(Box::new(Health::new(identity, ready, now))),
        };
        Card {
            id: identity.id.clone(),
            list_name: model.unwrap_or("Disco").into(),
            title: model.unwrap_or("Modelo desconhecido").into(),
            subtitle,
            body,
        }
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
        // Soft signals keep a passing disk from reading as plainly healthy, and
        // a score with no wear source behind it is not called healthy at all.
        let (label, tone) = match health.outcome {
            HealthOutcome::Pass if health.limited && health.warnings.is_empty() => {
                ("Limitada", Tone::Unknown)
            }
            HealthOutcome::Pass if health.warnings.is_empty() && health.score > 50 => {
                ("Saudável", Tone::Healthy)
            }
            HealthOutcome::Pass | HealthOutcome::Fail => ("Atenção", Tone::Attention),
            HealthOutcome::Unknown => ("Indisponível", Tone::Unknown),
        };
        let score = (health.outcome != HealthOutcome::Unknown).then(|| health.score.to_string());
        // A USB bridge that forwards nothing leaves identity and capacity only.
        let bridge_blocks_smart = is_usb(identity)
            && health.outcome == HealthOutcome::Unknown
            && !any_permission_failure(telemetry)
            && !matches!(&telemetry.nvme, QueryState::Success(_))
            && !matches!(&telemetry.ata, QueryState::Success(_))
            && !matches!(&telemetry.overall_failure, QueryState::Success(_));
        let status = match telemetry_status(telemetry) {
            _ if bridge_blocks_smart => USB_SMART_UNAVAILABLE,
            SMART_AVAILABLE | NO_GENERAL_FAILURE if tone == Tone::Unknown && health.limited => {
                SMART_LIMITED
            }
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
            advice: advice(&health, tone),
            details: details(telemetry),
            attributes: match &telemetry.ata {
                QueryState::Success(ata) => ata.attributes.iter().map(attribute_row).collect(),
                _ => Vec::new(),
            },
        }
    }
}

fn is_usb(identity: &DiskIdentity) -> bool {
    identity
        .bus
        .as_deref()
        .is_some_and(|bus| bus.eq_ignore_ascii_case("USB"))
}

/// Counts every ATA drive without a model profile reports the same way
/// (smartmontools' default layouts). A profiled drive's raw fields are decoded
/// by its profile, so they stay out of this list.
fn details(telemetry: &Telemetry) -> Vec<Detail> {
    let QueryState::Success(ata) = &telemetry.ata else {
        return Vec::new();
    };
    if telemetry.ata_profile.is_some() {
        return Vec::new();
    }
    let generic = generic_decode(ata);
    [
        ("Ciclos de energia", generic.power_cycles),
        ("Setores realocados", generic.reallocated_sectors),
        ("Eventos de realocação", generic.reallocated_events),
        ("Setores pendentes", generic.pending_sectors),
        (
            "Setores incorrigíveis offline",
            generic.offline_uncorrectable,
        ),
        (
            "Erros incorrigíveis reportados",
            generic.reported_uncorrectable,
        ),
        ("Erros de CRC na interface", generic.interface_crc_errors),
    ]
    .into_iter()
    .filter_map(|(label, count)| {
        count.map(|count| Detail {
            label,
            value: count.to_string(),
        })
    })
    .collect()
}

/// What to do next: back up on a verdict of Atenção, check the cable when that
/// is the only cause, and where to find the wear when the disk does not say.
fn advice(health: &HealthScore, tone: Tone) -> Option<&'static str> {
    match tone {
        Tone::Attention
            if health.outcome == HealthOutcome::Pass
                && health.warnings == [HealthWarningReason::AtaInterfaceCrc] =>
        {
            Some(CABLE_ADVICE)
        }
        Tone::Attention => Some(BACKUP_ADVICE),
        Tone::Unknown if health.limited => Some(LIMITED_ADVICE),
        Tone::Healthy | Tone::Unknown => None,
    }
}

const BACKUP_ADVICE: &str = "Faça backup dos dados importantes.";
const CABLE_ADVICE: &str =
    "Troque o cabo de dados ou a porta e confira se o número de erros continua subindo.";
const LIMITED_ADVICE: &str =
    "Use o programa do fabricante do SSD para ver o desgaste, que este disco não informa.";

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
            HealthWarningReason::AtaInterfaceCrc => lines.push(
                "Erros de CRC na interface: a falha costuma estar no cabo ou na conexão, não no disco"
                    .into(),
            ),
            HealthWarningReason::AtaCountersCritical => lines.push(
                "Quantidade alta de setores com defeito; copie seus dados e considere trocar o disco"
                    .into(),
            ),
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
    let name = match id {
        0x01 => "Taxa de erros de leitura",
        0x03 => "Tempo de aceleração do disco",
        0x04 => "Ciclos de partida e parada",
        0x05 => "Setores realocados",
        0x07 => "Taxa de erros de posicionamento",
        0x09 => "Horas ligado",
        0x0A => "Tentativas de giro",
        0x0C => "Ciclos de energia",
        0xBB => "Erros incorrigíveis reportados",
        0xBC => "Tempos limite de comando",
        0xBE => "Temperatura do fluxo de ar",
        0xC2 => "Temperatura",
        0xC4 => "Eventos de realocação",
        0xC5 => "Setores pendentes",
        0xC6 => "Setores incorrigíveis offline",
        0xC7 => "Erros de CRC na interface",
        _ => "Atributo ATA",
    };
    format!("{name} ({id:02X})")
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

const USB_SMART_UNAVAILABLE: &str = "SMART indisponível por este adaptador USB";

/// A USB bridge that does not forward SMART leaves only the identity: say so
/// and what to try, instead of calling the disk incompatible.
pub fn query_error_message_for(identity: &DiskIdentity, state: QueryErrorState) -> String {
    if state == QueryErrorState::Unsupported && is_usb(identity) {
        format!(
            "{USB_SMART_UNAVAILABLE}. O disco foi encontrado, mas o adaptador não repassa os dados de saúde. Para ver o SMART, conecte-o direto à porta SATA ou NVMe."
        )
    } else {
        query_error_message(state)
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
const NO_GENERAL_FAILURE: &str = "Sem falha geral relatada pelo dispositivo";
const SMART_LIMITED: &str = "Avaliação limitada: o disco não informa desgaste";
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
        return NO_GENERAL_FAILURE;
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
            (worn, "Atenção", Tone::Attention),
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
                assert_eq!(health.score, Some(expected.score.to_string()));
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
    fn a_passing_disk_with_soft_signals_reads_as_attention_and_says_why() {
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
        assert_eq!((health.label, health.tone), ("Atenção", Tone::Attention));
        assert!(health.score.is_some());
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
        assert_eq!(health.label, "Atenção");
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
        assert_eq!((health.label, health.tone), ("Atenção", Tone::Attention));
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

    fn row(id: u8, raw: u64) -> AtaAttribute {
        AtaAttribute {
            id,
            flags: 0,
            current: 100,
            worst: 100,
            threshold: Some(0),
            raw: u64::to_le_bytes(raw)[..6].try_into().unwrap(),
        }
    }

    fn unprofiled(rows: Vec<AtaAttribute>) -> Telemetry {
        let mut telemetry = Telemetry {
            overall_failure: QueryState::Success(false),
            ..Default::default()
        };
        telemetry.apply_ata(
            AtaHealth {
                attributes: rows,
                thresholds_available: true,
            },
            Some("MACROVIP MV240GB"),
        );
        telemetry
    }

    #[test]
    fn a_generic_sata_ssd_shows_hours_cycles_and_a_limited_assessment_not_a_healthy_100() {
        // What the MV240GB reports: 09 = 0x423C, 0C = 0x832, no 194 and no wear.
        let telemetry = unprofiled(vec![
            row(0x01, 0),
            row(0x05, 0),
            row(0x09, 0x423C),
            row(0x0C, 0x832),
            row(0xA0, 0),
        ]);
        let health = health_of(telemetry, DiskKind::Ssd);
        assert_eq!(health.power_on, "16956 h");
        assert_eq!(health.temperature, "indisponível");
        assert_eq!(health.wear, "indisponível");
        assert_eq!((health.label, health.tone), ("Limitada", Tone::Unknown));
        assert_ne!(health.label, "Saudável");
        assert_eq!(health.score.as_deref(), Some("100"));
        assert_eq!(
            health.status,
            "Avaliação limitada: o disco não informa desgaste"
        );
        assert_eq!(health.advice, Some(LIMITED_ADVICE));
        assert!(health.warnings.is_empty());
        let detail = |label: &str| {
            health
                .details
                .iter()
                .find(|detail| detail.label == label)
                .map(|detail| detail.value.as_str())
        };
        assert_eq!(detail("Ciclos de energia"), Some("2098"));
        assert_eq!(detail("Setores realocados"), Some("0"));
        assert_eq!(
            detail("Setores pendentes"),
            None,
            "absent rows are not invented"
        );
    }

    #[test]
    fn the_default_temperature_attribute_is_shown_and_a_profiled_disk_lists_no_generic_counters() {
        let health = health_of(
            unprofiled(vec![row(0x05, 0), row(0xC2, 0x0037_0014_0029)]),
            DiskKind::Ssd,
        );
        assert_eq!(health.temperature, "41 °C");

        let mut wd = Telemetry::default();
        wd.apply_ata(
            AtaHealth {
                attributes: vec![row(0x05, 0), row(0x0C, 5)],
                thresholds_available: false,
            },
            Some("WDC WD10EZEX-00WN4A0"),
        );
        assert!(health_of(wd, DiskKind::Hdd).details.is_empty());
    }

    #[test]
    fn raised_sector_counters_are_values_and_pt_br_warnings_with_a_next_step() {
        let health = health_of(
            unprofiled(vec![row(0x05, 3), row(0xC5, 1), row(0xBB, 2)]),
            DiskKind::Ssd,
        );
        assert_eq!((health.label, health.tone), ("Atenção", Tone::Attention));
        assert_eq!(
            health.warnings,
            [
                "Setores realocados registrados na mídia",
                "Setores pendentes aguardando remapeamento",
                "Erros incorrigíveis reportados pelo dispositivo",
            ]
        );
        assert_eq!(health.advice, Some(BACKUP_ADVICE));
        let values: Vec<_> = health
            .details
            .iter()
            .map(|detail| (detail.label, detail.value.as_str()))
            .collect();
        assert_eq!(
            values,
            [
                ("Setores realocados", "3"),
                ("Setores pendentes", "1"),
                ("Erros incorrigíveis reportados", "2"),
            ]
        );

        let critical = health_of(unprofiled(vec![row(0x05, 150)]), DiskKind::Ssd);
        assert_eq!(critical.label, "Atenção");
        assert!(critical.warnings.contains(
            &"Quantidade alta de setores com defeito; copie seus dados e considere trocar o disco"
                .to_string()
        ));
    }

    #[test]
    fn interface_crc_errors_point_at_the_cable_not_the_drive() {
        let health = health_of(unprofiled(vec![row(0x05, 0), row(0xC7, 12)]), DiskKind::Ssd);
        assert_eq!((health.label, health.tone), ("Atenção", Tone::Attention));
        assert_eq!(
            health.warnings,
            [
                "Erros de CRC na interface: a falha costuma estar no cabo ou na conexão, não no disco"
            ]
        );
        assert_eq!(health.advice, Some(CABLE_ADVICE));
        assert_eq!(health.score.as_deref(), Some("100"), "not drive wear");
        assert!(
            health
                .details
                .iter()
                .any(|detail| detail.label == "Erros de CRC na interface" && detail.value == "12")
        );
    }

    #[test]
    fn a_usb_disk_without_smart_shows_identity_a_clear_message_and_no_score() {
        let mut stick = identity("usb");
        stick.model = Some("Lexar USB Flash Drive".into());
        stick.bus = Some("USB".into());
        stick.capacity_bytes = Some(32_010_928_128);

        // Nothing reachable: the whole query was refused as unsupported.
        let unreachable = card(stick.clone(), Reading::Failed(QueryErrorState::Unsupported));
        assert_eq!(unreachable.title, "Lexar USB Flash Drive");
        assert_eq!(unreachable.subtitle, "USB  ·  29,8 GiB");
        let Body::Message(message) = &unreachable.body else {
            panic!("an unreadable disk shows a message");
        };
        assert!(
            message.starts_with("SMART indisponível por este adaptador USB"),
            "{message}"
        );
        assert!(message.contains("conecte-o direto"), "{message}");
        assert_eq!(unreachable.status_label(), "Indisponível");
        assert_eq!(unreachable.tone(), Some(Tone::Unknown));

        // The same refusal on an internal disk keeps the generic wording.
        let mut internal = stick.clone();
        internal.bus = Some("SATA".into());
        let internal = card(internal, Reading::Failed(QueryErrorState::Unsupported));
        assert_eq!(
            internal.body,
            Body::Message(query_error_message(QueryErrorState::Unsupported))
        );
        // A transient failure over USB is still retried, not blamed on the adapter.
        let retry = card(stick.clone(), Reading::Failed(QueryErrorState::Transient));
        assert_eq!(
            retry.body,
            Body::Message(query_error_message(QueryErrorState::Transient))
        );

        // Only a temperature came back: still no score, and the status says why.
        let only_temperature = Telemetry {
            temperature_c: Some(34),
            ..Default::default()
        };
        let Body::Health(health) = card(stick.clone(), ready(only_temperature)).body else {
            panic!("a reading must produce a health body");
        };
        assert_eq!((health.label, health.score), ("Indisponível", None));
        assert_eq!(health.status, "SMART indisponível por este adaptador USB");
        assert_eq!(health.temperature, "34 °C");

        // A known kind over USB keeps its kind and names the bus.
        stick.kind = DiskKind::Ssd;
        assert_eq!(
            card(stick, Reading::Pending).subtitle,
            "SSD  ·  USB  ·  29,8 GiB"
        );
    }
}
