//! Conservative interpretations of ATA SMART data.
//!
//! Raw values remain opaque unless a standard HDD signal is applicable or an
//! exact vendor/model profile is supported by authoritative documentation.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ModelProfile {
    pub family: &'static str,
    pub lifetime_used_attribute: u8,
    pub lifetime_used_label_pt_br: &'static str,
    pub lifetime_remaining_label_pt_br: &'static str,
}

/// Narrow HDD profile used to interpret selected ATA raw counters. SMART raw
/// fields are vendor-defined; this profile deliberately covers only the WD
/// Blue WD10EZEX family visible in the user's hardware sample.
pub(crate) const WD10EZEX_PROFILE: &str = "Western Digital WD10EZEX";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Raw5Encoding {
    /// The primary low word is a count; upper words are optional subfields
    /// and must be zero until their meaning has a separate verified mapping.
    Raw16PrimaryOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PowerOnHoursEncoding {
    Raw24Hours,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TemperatureEncoding {
    TempMinMaxCurrentByte,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HddModelProfile {
    pub family: &'static str,
    model_prefix: &'static str,
    raw5_encoding: Raw5Encoding,
    power_on_hours_encoding: PowerOnHoursEncoding,
    temperature_encoding: TemperatureEncoding,
}

// In smartmontools' upstream drivedb v8.0, the DEFAULT preset maps ID 9 to
// `raw24(raw8),Power_On_Hours` and ID 194 to `tempminmax,Temperature_Celsius`.
// The checked database has no WD10EZEX-specific override, so this exact model
// profile declares those default layouts explicitly rather than guessing for
// other models/vendors.
const HDD_MODEL_PROFILES: &[HddModelProfile] = &[HddModelProfile {
    family: WD10EZEX_PROFILE,
    model_prefix: "WD10EZEX",
    raw5_encoding: Raw5Encoding::Raw16PrimaryOnly,
    power_on_hours_encoding: PowerOnHoursEncoding::Raw24Hours,
    temperature_encoding: TemperatureEncoding::TempMinMaxCurrentByte,
}];

/// Raw problem counters shared by every ATA drive (smartmontools DEFAULT entry).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AtaCounter {
    ReallocatedSectors,
    SpinRetries,
    ReportedUncorrectable,
    PendingSectors,
    OfflineUncorrectable,
    /// UDMA CRC errors count transfers corrupted on the cable, not media wear.
    InterfaceCrc,
}

impl AtaCounter {
    pub(crate) const fn id(self) -> u8 {
        match self {
            Self::ReallocatedSectors => 5,
            Self::SpinRetries => 10,
            Self::ReportedUncorrectable => 187,
            Self::PendingSectors => 197,
            Self::OfflineUncorrectable => 198,
            Self::InterfaceCrc => 199,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::ReallocatedSectors => "reallocated sectors",
            Self::SpinRetries => "spin retries",
            Self::ReportedUncorrectable => "reported uncorrectable errors",
            Self::PendingSectors => "pending sectors",
            Self::OfflineUncorrectable => "offline uncorrectable sectors",
            Self::InterfaceCrc => "interface CRC errors",
        }
    }

    /// Whether the counter measures the drive itself. Interface CRC errors
    /// point at the cable or bridge and never lower the score.
    pub(crate) const fn is_drive_wear(self) -> bool {
        !matches!(self, Self::InterfaceCrc)
    }

    const fn from_id(id: u8) -> Option<Self> {
        match id {
            5 => Some(Self::ReallocatedSectors),
            10 => Some(Self::SpinRetries),
            187 => Some(Self::ReportedUncorrectable),
            197 => Some(Self::PendingSectors),
            198 => Some(Self::OfflineUncorrectable),
            199 => Some(Self::InterfaceCrc),
            _ => None,
        }
    }
}

/// Return an ATA row only when that ID appears exactly once. This is required
/// before interpreting model-specific SMART data such as lifetime-used ID 202.
pub fn unique_attribute(
    health: &crate::model::AtaHealth,
    id: u8,
) -> Option<&crate::model::AtaAttribute> {
    let mut matching = health
        .attributes
        .iter()
        .filter(|attribute| attribute.id == id);
    let attribute = matching.next()?;
    matching.next().is_none().then_some(attribute)
}

/// Model-scoped HDD layouts are intentionally explicit. Unknown models have
/// no model profile; only the limited common HDD signal defaults below can
/// still be used, never a guessed vendor health baseline. ATA ID 5 uses
/// smartmontools' `raw16(raw16)` default convention: its primary count is
/// accepted only when optional upper words are zero, so no high bytes are
/// silently truncated. Other listed counters are raw48.
pub(crate) fn hdd_profile(model: Option<&str>) -> Option<&'static HddModelProfile> {
    let model = model?.trim().to_ascii_uppercase();
    let model = model
        .strip_prefix("ATA_")
        .or_else(|| model.strip_prefix("ATA "))
        .unwrap_or(&model)
        .trim();
    let model = model.strip_prefix("WDC ").unwrap_or(model).trim();
    HDD_MODEL_PROFILES
        .iter()
        .find(|profile| model.starts_with(profile.model_prefix))
}

/// The counter an ATA ID is scored as for this disk, or `None` when the ID is
/// not scored. A drive with no model match gets the vendor-independent
/// counters whatever its kind (NVMe aside); spin retries only make sense for
/// a spindle. The WD profile keeps its HDD-only set, and 199 is never scored
/// there.
pub(crate) fn counter_applies(
    kind: crate::model::DiskKind,
    profile_family: Option<&str>,
    id: u8,
) -> Option<AtaCounter> {
    let counter = AtaCounter::from_id(id)?;
    let applies = match profile_family {
        Some(family) => {
            kind == crate::model::DiskKind::Hdd
                && counter != AtaCounter::InterfaceCrc
                && HDD_MODEL_PROFILES
                    .iter()
                    .any(|profile| profile.family == family)
        }
        None => match counter {
            AtaCounter::SpinRetries => kind == crate::model::DiskKind::Hdd,
            _ => kind != crate::model::DiskKind::Nvme,
        },
    };
    applies.then_some(counter)
}

pub(crate) fn scored_counter(
    kind: crate::model::DiskKind,
    profile_family: Option<&str>,
    id: u8,
    raw: [u8; 6],
) -> Option<(AtaCounter, u64)> {
    let counter = counter_applies(kind, profile_family, id)?;
    let raw5_encoding = match profile_family {
        Some(family) => {
            HDD_MODEL_PROFILES
                .iter()
                .find(|profile| profile.family == family)?
                .raw5_encoding
        }
        // The smartmontools DEFAULT entry, shared by drives with no profile.
        None => Raw5Encoding::Raw16PrimaryOnly,
    };
    let value = match (counter, raw5_encoding) {
        // ID 5 follows raw16(raw16): the low word is the count and the upper
        // words are optional subfields. Accept only an unambiguous primary
        // count, so packed fields are neither interpreted nor silently
        // truncated.
        (AtaCounter::ReallocatedSectors, Raw5Encoding::Raw16PrimaryOnly) => {
            if raw[2..].iter().any(|byte| *byte != 0) {
                return None;
            }
            u64::from(u16::from_le_bytes([raw[0], raw[1]]))
        }
        _ => raw_u48(raw),
    };
    Some((counter, value))
}

/// Vendor-independent reading of the ATA attributes that have the same meaning
/// on (nearly) every drive: smartmontools' DEFAULT drivedb entry. Each field is
/// absent when its ID is missing or appears more than once, since a duplicated
/// row has no authoritative value. Raw fields are decoded as that entry
/// declares: 9 raw24 (the upper bytes may hold a vendor sub-unit and are
/// ignored), 5 and 196 raw16, 194/190 the low byte only (min/max bytes follow
/// it), everything else raw48. SSD life attributes (169, 173, 177, 202, 231,
/// 232, 233, 0xA4-0xA8) are vendor-defined and deliberately not decoded here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GenericAta {
    pub power_on_hours: Option<u64>,
    pub power_cycles: Option<u64>,
    pub temperature_c: Option<i16>,
    pub reallocated_sectors: Option<u64>,
    pub reallocated_events: Option<u64>,
    pub pending_sectors: Option<u64>,
    pub offline_uncorrectable: Option<u64>,
    pub interface_crc_errors: Option<u64>,
    pub reported_uncorrectable: Option<u64>,
    pub command_timeouts: Option<u64>,
}

pub fn generic_decode(health: &crate::model::AtaHealth) -> GenericAta {
    let raw = |id| unique_attribute(health, id).map(|attribute| attribute.raw);
    let raw48 = |id| raw(id).map(raw_u48);
    let raw16 = |id| raw(id).map(|raw| u64::from(u16::from_le_bytes([raw[0], raw[1]])));
    // 194 wins over 190 (airflow); an implausible byte is not a temperature.
    let temperature = |id| {
        raw(id)
            .map(|raw| raw[0])
            .filter(|celsius| (1..=100).contains(celsius))
            .map(i16::from)
    };
    GenericAta {
        power_on_hours: raw(9).map(|raw| raw_u48(raw) & 0x00ff_ffff),
        power_cycles: raw48(12),
        temperature_c: temperature(194).or_else(|| temperature(190)),
        reallocated_sectors: raw16(5),
        reallocated_events: raw16(196),
        pending_sectors: raw48(197),
        offline_uncorrectable: raw48(198),
        interface_crc_errors: raw48(199),
        reported_uncorrectable: raw48(187),
        command_timeouts: raw48(188),
    }
}

/// Decode the common SATA HDD temperature/hour fields only for a registered
/// model family. Unknown/vendor-specific layouts remain absent.
pub(crate) fn hdd_power_on_hours(
    model: Option<&str>,
    health: &crate::model::AtaHealth,
) -> Option<u64> {
    let profile = hdd_profile(model)?;
    if profile.power_on_hours_encoding != PowerOnHoursEncoding::Raw24Hours {
        return None;
    }
    let mut matches = health
        .attributes
        .iter()
        .filter(|attribute| attribute.id == 9);
    let attribute = matches.next()?;
    if matches.next().is_some() || attribute.raw[3..].iter().any(|byte| *byte != 0) {
        return None;
    }
    Some(
        u64::from(attribute.raw[0])
            | (u64::from(attribute.raw[1]) << 8)
            | (u64::from(attribute.raw[2]) << 16),
    )
}

pub(crate) fn hdd_temperature_c(
    model: Option<&str>,
    health: &crate::model::AtaHealth,
) -> Option<i16> {
    let profile = hdd_profile(model)?;
    if profile.temperature_encoding != TemperatureEncoding::TempMinMaxCurrentByte {
        return None;
    }
    let mut matches = health
        .attributes
        .iter()
        .filter(|attribute| attribute.id == 194);
    let attribute = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let celsius = attribute.raw[0];
    (1..=100).contains(&celsius).then_some(i16::from(celsius))
}

/// Exact Crucial/Micron families from the upstream smartmontools drive DB,
/// checked against Micron TN-FD-22: SMART 202's full 48-bit raw value is
/// percent used.
/// The normalized field is `max(100 - raw, 0)` and is not the used percent.
pub(crate) fn profile(model: Option<&str>) -> Option<ModelProfile> {
    let model = model?.trim().to_ascii_uppercase();
    // Some generic storage descriptors prepend their vendor as "ATA". Strip
    // only that exact token, then the known Crucial brand token; keep the
    // original identity untouched for display and reject arbitrary prefixes.
    let model = model
        .strip_prefix("ATA_")
        .or_else(|| model.strip_prefix("ATA "))
        .unwrap_or(&model)
        .trim();
    let product = model
        .strip_prefix("CRUCIAL_")
        .or_else(|| model.strip_prefix("CRUCIAL "))
        .unwrap_or(model)
        .trim();
    if !product.starts_with("CT") {
        return None;
    }
    let token = product
        .split(|character: char| !character.is_ascii_alphanumeric())
        .next()?;
    let family = if [
        "CT275MX300SSD1",
        "CT275MX300SSD4",
        "CT525MX300SSD1",
        "CT525MX300SSD4",
        "CT750MX300SSD1",
        "CT750MX300SSD4",
        "CT1050MX300SSD1",
        "CT1050MX300SSD4",
        "CT2050MX300SSD1",
        "CT2050MX300SSD4",
    ]
    .contains(&token)
    {
        Some("Crucial MX300")
    } else if [
        "CT250MX500SSD",
        "CT500MX500SSD",
        "CT1000MX500SSD",
        "CT2000MX500SSD",
        "CT4000MX500SSD",
    ]
    .iter()
    .any(|prefix| {
        ["1", "4"].iter().any(|form| {
            token.strip_prefix(prefix).is_some_and(|tail| {
                tail.strip_prefix(form)
                    .is_some_and(is_optional_one_letter_suffix)
            })
        })
    }) {
        Some("Crucial MX500")
    } else if [
        "CT120BX500SSD1",
        "CT240BX500SSD1",
        "CT480BX500SSD1",
        "CT960BX500SSD1",
        "CT1000BX500SSD1",
        "CT2000BX500SSD1",
        "CT4000BX500SSD1",
    ]
    .contains(&token)
    {
        Some("Crucial BX500")
    } else {
        None
    };
    Some(ModelProfile {
        family: family?,
        lifetime_used_attribute: 202,
        lifetime_used_label_pt_br: "Vida útil usada",
        lifetime_remaining_label_pt_br: "Vida útil restante",
    })
}

fn is_optional_one_letter_suffix(suffix: &str) -> bool {
    suffix.is_empty() || (suffix.len() == 1 && suffix.as_bytes()[0].is_ascii_alphabetic())
}

pub(crate) fn endurance_used_percent(model: Option<&str>, id: u8, raw: [u8; 6]) -> Option<u8> {
    let profile = profile(model)?;
    (id == profile.lifetime_used_attribute).then(|| raw_u48(raw).min(100) as u8)
}

/// Short, user-facing Brazilian Portuguese labels for supported attributes.
pub(crate) fn attribute_name_pt_br(model: Option<&str>, id: u8) -> Option<&'static str> {
    if id == 202 {
        return profile(model).map(|profile| profile.lifetime_remaining_label_pt_br);
    }
    None
}

#[allow(dead_code)] // Exposed for the UI's raw-value column when it is rendered.
pub(crate) fn raw_attribute_name_pt_br(model: Option<&str>, id: u8) -> Option<&'static str> {
    if id == 202 {
        return profile(model).map(|profile| profile.lifetime_used_label_pt_br);
    }
    attribute_name_pt_br(model, id)
}

pub(crate) fn raw_u48(raw: [u8; 6]) -> u64 {
    u64::from_le_bytes([raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], 0, 0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_attribute_lookup_rejects_equal_or_different_duplicate_rows() {
        let attribute = |raw| crate::model::AtaAttribute {
            id: 202,
            flags: 0,
            current: 77,
            worst: 75,
            threshold: None,
            raw: [raw, 0, 0, 0, 0, 0],
        };
        let one = crate::model::AtaHealth {
            attributes: vec![attribute(23)],
            thresholds_available: false,
        };
        assert_eq!(unique_attribute(&one, 202).unwrap().raw[0], 23);
        for values in [[23, 23], [23, 81], [81, 23]] {
            let duplicate = crate::model::AtaHealth {
                attributes: values.into_iter().map(attribute).collect(),
                thresholds_available: false,
            };
            assert_eq!(unique_attribute(&duplicate, 202), None, "rows {values:?}");
        }
        assert_eq!(unique_attribute(&one, 5), None);
    }

    #[test]
    fn raw_hdd_ids_are_unlabeled_without_a_model_profile() {
        for id in [5, 9, 177, 194, 197, 198, 202] {
            assert_eq!(attribute_name_pt_br(Some("MacroVIP MV240GB"), id), None);
            assert_eq!(raw_attribute_name_pt_br(Some("MacroVIP MV240GB"), id), None);
        }
    }

    #[test]
    fn wd10ezex_profile_is_exact_and_common_hdd_counters_keep_full_raw_width() {
        let profile = hdd_profile(Some("WDC WD10EZEX-00WN4A0")).unwrap();
        assert_eq!(profile.family, WD10EZEX_PROFILE);
        assert_eq!(
            hdd_profile(Some("ATA WDC WD10EZEX-00WN4A0"))
                .unwrap()
                .family,
            WD10EZEX_PROFILE
        );
        assert_eq!(hdd_profile(Some("WDC WD10EZRX-00DC0B0")), None);
        assert_eq!(hdd_profile(Some("Other WDC WD10EZEX-00WN4A0")), None);

        let raw_65_536 = [0, 0, 1, 0, 0, 0];
        assert_eq!(
            scored_counter(
                crate::model::DiskKind::Hdd,
                Some(WD10EZEX_PROFILE),
                197,
                raw_65_536,
            ),
            Some((AtaCounter::PendingSectors, 65_536))
        );
        assert_eq!(
            scored_counter(
                crate::model::DiskKind::Hdd,
                Some(WD10EZEX_PROFILE),
                197,
                [255; 6],
            ),
            Some((AtaCounter::PendingSectors, 0x0000_ffff_ffff_ffff))
        );
        assert_eq!(
            scored_counter(
                crate::model::DiskKind::Hdd,
                Some(WD10EZEX_PROFILE),
                5,
                [3, 0, 0, 0, 0, 0],
            ),
            Some((AtaCounter::ReallocatedSectors, 3))
        );
        // ID 5 follows raw16(raw16), so extra words are ambiguous and cannot
        // be silently dropped to a falsely small primary count.
        assert_eq!(
            scored_counter(
                crate::model::DiskKind::Hdd,
                Some(WD10EZEX_PROFILE),
                5,
                [1, 0, 1, 0, 0, 0],
            ),
            None
        );
        assert_eq!(
            scored_counter(
                crate::model::DiskKind::Ssd,
                Some(WD10EZEX_PROFILE),
                197,
                [1, 0, 0, 0, 0, 0],
            ),
            None
        );
        // Vendor-packed read/seek rates and unrecognized attributes stay out.
        for id in [1, 7, 9, 177, 202] {
            assert_eq!(
                scored_counter(
                    crate::model::DiskKind::Hdd,
                    Some(WD10EZEX_PROFILE),
                    id,
                    [9, 0, 0, 0, 0, 0],
                ),
                None,
                "attribute {id}"
            );
        }
        assert_eq!(
            scored_counter(crate::model::DiskKind::Hdd, None, 197, raw_65_536,),
            Some((AtaCounter::PendingSectors, 65_536)),
            "documented common HDD defaults apply without assuming a model profile"
        );
    }

    #[test]
    fn hdd_temperature_and_hours_are_decoded_only_for_the_supported_wdc_profile() {
        let health = crate::model::AtaHealth {
            attributes: vec![
                crate::model::AtaAttribute {
                    id: 9,
                    flags: 0,
                    current: 100,
                    worst: 100,
                    threshold: None,
                    raw: [0x07, 0x1a, 0, 0, 0, 0],
                },
                crate::model::AtaAttribute {
                    id: 194,
                    flags: 0,
                    current: 100,
                    worst: 100,
                    threshold: None,
                    raw: [40, 55, 63, 0, 0, 0],
                },
            ],
            thresholds_available: false,
        };
        let model = Some("WDC WD10EZEX-00WN4A0");
        assert_eq!(hdd_power_on_hours(model, &health), Some(6663));
        assert_eq!(hdd_temperature_c(model, &health), Some(40));
        assert_eq!(
            hdd_power_on_hours(Some("Unverified WDC HDD"), &health),
            None
        );
        assert_eq!(hdd_temperature_c(Some("Unverified WDC HDD"), &health), None);

        let ambiguous_hours = crate::model::AtaHealth {
            attributes: vec![crate::model::AtaAttribute {
                raw: [0x07, 0x1a, 0, 1, 0, 0],
                ..health.attributes[0].clone()
            }],
            thresholds_available: false,
        };
        assert_eq!(hdd_power_on_hours(model, &ambiguous_hours), None);
        let invalid_temperature = crate::model::AtaHealth {
            attributes: vec![crate::model::AtaAttribute {
                raw: [101, 0, 0, 0, 0, 0],
                ..health.attributes[1].clone()
            }],
            thresholds_available: false,
        };
        assert_eq!(hdd_temperature_c(model, &invalid_temperature), None);
    }

    #[test]
    fn crucials_supported_202_raw_values_are_used_not_remaining_and_models_are_exact() {
        for model in [
            "CT275MX300SSD1",
            "Crucial CT1050MX300SSD4",
            "ATA CT500MX500SSD1",
            "ATA_CT500MX500SSD1",
            "ATA Crucial_CT500MX500SSD1",
            "Crucial_CT500MX500SSD1",
            "CT500MX500SSD1A",
            "CT2000MX500SSD4",
            "CT4000MX500SSD1",
            "ATA CT4000MX500SSD1",
            "ATA_CT4000MX500SSD1",
            "CT120BX500SSD1",
            "CT4000BX500SSD1",
        ] {
            let profile = profile(Some(model)).unwrap();
            assert_eq!(profile.lifetime_used_attribute, 202);
            assert_eq!(profile.lifetime_used_label_pt_br, "Vida útil usada");
            assert_eq!(profile.lifetime_remaining_label_pt_br, "Vida útil restante");
            assert_eq!(
                endurance_used_percent(Some(model), 202, [23, 0, 0, 0, 0, 0]),
                Some(23)
            );
            assert_eq!(
                attribute_name_pt_br(Some(model), 202),
                Some("Vida útil restante")
            );
            assert_eq!(
                raw_attribute_name_pt_br(Some(model), 202),
                Some("Vida útil usada")
            );
        }
        assert_eq!(profile(Some("Crucial MX500")), None);
        assert_eq!(profile(Some("CT480MX300SSD1")), None);
        assert_eq!(profile(Some("CT500MX500SSD2")), None);
        assert_eq!(profile(Some("CT120BX500SSD4")), None);
        assert_eq!(profile(Some("Other CT500MX500SSD1")), None);
        assert_eq!(profile(Some("ATAx CT500MX500SSD1")), None);
        assert_eq!(profile(Some("ATA_CT4000MX500SSD2")), None);
        assert_eq!(profile(Some("CT4000MX500SSD2")), None);
        assert_eq!(profile(Some("MacroVIP MV240GB")), None);
        assert_eq!(profile(Some("Samsung SSD 870 EVO")), None);
        assert_eq!(
            endurance_used_percent(Some("CT250MX500SSD1"), 202, [255; 6]),
            Some(100)
        );
        // Raw is a 48-bit little-endian value; using only byte 0 would
        // misread 256 percent used as zero and report a perfect drive.
        assert_eq!(raw_u48([0, 1, 0, 0, 0, 0]), 256);
        assert_eq!(
            endurance_used_percent(Some("CT250MX500SSD1"), 202, [0, 1, 0, 0, 0, 0]),
            Some(100)
        );
        assert_eq!(
            endurance_used_percent(Some("CT250MX500SSD1"), 177, [23; 6]),
            None
        );
    }

    fn health_of(rows: &[(u8, [u8; 6])]) -> crate::model::AtaHealth {
        crate::model::AtaHealth {
            attributes: rows
                .iter()
                .map(|(id, raw)| crate::model::AtaAttribute {
                    id: *id,
                    flags: 0,
                    current: 100,
                    worst: 100,
                    threshold: None,
                    raw: *raw,
                })
                .collect(),
            thresholds_available: false,
        }
    }

    #[test]
    fn generic_decode_reads_the_smartmontools_default_layouts() {
        // 9 is raw24: the upper bytes are ignored. 0x423C is the MV240GB's.
        let hours = generic_decode(&health_of(&[(9, [0x3C, 0x42, 0, 0xAA, 0xBB, 0xCC])]));
        assert_eq!(hours.power_on_hours, Some(16_956));
        let hours = generic_decode(&health_of(&[(9, [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])]));
        assert_eq!(hours.power_on_hours, Some(0x00ff_ffff));

        // 12 is raw48; 0x832 is the MV240GB's.
        let cycles = generic_decode(&health_of(&[(12, [0x32, 0x08, 0, 0, 0, 0])]));
        assert_eq!(cycles.power_cycles, Some(2_098));
        let cycles = generic_decode(&health_of(&[(12, [0, 0, 0, 0, 0, 1])]));
        assert_eq!(cycles.power_cycles, Some(1 << 40));

        // 194 and 190 keep the temperature in byte 0; min and max follow it.
        let with_min_max = [40, 0, 20, 0, 55, 0];
        let temperature = generic_decode(&health_of(&[(194, with_min_max)]));
        assert_eq!(temperature.temperature_c, Some(40));
        let airflow = generic_decode(&health_of(&[(190, [33, 0, 0, 0, 0, 0])]));
        assert_eq!(airflow.temperature_c, Some(33));
        // 194 wins over 190; a bad or duplicated 194 falls back to 190.
        let both = generic_decode(&health_of(&[
            (190, [33, 0, 0, 0, 0, 0]),
            (194, with_min_max),
        ]));
        assert_eq!(both.temperature_c, Some(40));
        let bad_194 = generic_decode(&health_of(&[
            (194, [0, 0, 0, 0, 0, 0]),
            (190, [33, 0, 0, 0, 0, 0]),
        ]));
        assert_eq!(bad_194.temperature_c, Some(33));
        let duplicate_194 = generic_decode(&health_of(&[
            (194, with_min_max),
            (194, [41, 0, 0, 0, 0, 0]),
            (190, [33, 0, 0, 0, 0, 0]),
        ]));
        assert_eq!(duplicate_194.temperature_c, Some(33));
        for implausible in [0u8, 101, 200, 255] {
            let none = generic_decode(&health_of(&[(194, [implausible, 0, 20, 0, 55, 0])]));
            assert_eq!(none.temperature_c, None, "{implausible} degrees");
        }
        assert_eq!(
            generic_decode(&health_of(&[(194, [100, 0, 0, 0, 0, 0])])).temperature_c,
            Some(100)
        );

        // 5 and 196 are raw16; the upper words are not part of the count.
        let raw16 = [3, 1, 9, 9, 9, 9];
        let reallocated = generic_decode(&health_of(&[(5, raw16), (196, raw16)]));
        assert_eq!(reallocated.reallocated_sectors, Some(259));
        assert_eq!(reallocated.reallocated_events, Some(259));

        // The rest are raw48.
        let wide = [1, 2, 3, 4, 5, 6];
        let expected = 0x0605_0403_0201;
        let counters = generic_decode(&health_of(&[
            (197, wide),
            (198, wide),
            (199, wide),
            (187, wide),
            (188, wide),
        ]));
        assert_eq!(counters.pending_sectors, Some(expected));
        assert_eq!(counters.offline_uncorrectable, Some(expected));
        assert_eq!(counters.interface_crc_errors, Some(expected));
        assert_eq!(counters.reported_uncorrectable, Some(expected));
        assert_eq!(counters.command_timeouts, Some(expected));
    }

    #[test]
    fn generic_decode_leaves_missing_and_duplicated_rows_absent() {
        assert_eq!(generic_decode(&health_of(&[])), GenericAta::default());
        let duplicated = generic_decode(&health_of(&[
            (9, [1, 0, 0, 0, 0, 0]),
            (9, [2, 0, 0, 0, 0, 0]),
            (12, [5, 0, 0, 0, 0, 0]),
            (197, [1, 0, 0, 0, 0, 0]),
            (197, [1, 0, 0, 0, 0, 0]),
        ]));
        assert_eq!(duplicated.power_on_hours, None);
        assert_eq!(duplicated.pending_sectors, None);
        assert_eq!(duplicated.power_cycles, Some(5));
        // Vendor-defined SSD life attributes are not decoded here.
        let ssd = generic_decode(&health_of(&[(177, [9; 6]), (202, [9; 6]), (231, [9; 6])]));
        assert_eq!(ssd, GenericAta::default());
    }

    #[test]
    fn generic_counters_score_any_non_nvme_drive_without_a_profile_and_nothing_else() {
        use crate::model::DiskKind::{Hdd, Nvme, Ssd, Unknown};
        for kind in [Hdd, Ssd, Unknown] {
            for (id, counter) in [
                (5, AtaCounter::ReallocatedSectors),
                (187, AtaCounter::ReportedUncorrectable),
                (197, AtaCounter::PendingSectors),
                (198, AtaCounter::OfflineUncorrectable),
                (199, AtaCounter::InterfaceCrc),
            ] {
                assert_eq!(
                    scored_counter(kind, None, id, [2, 0, 0, 0, 0, 0]),
                    Some((counter, 2)),
                    "{kind:?} {id}"
                );
            }
        }
        // Spin retries describe a spindle.
        assert!(counter_applies(Hdd, None, 10).is_some());
        assert_eq!(counter_applies(Ssd, None, 10), None);
        assert_eq!(counter_applies(Nvme, None, 197), None);
        // Hours, cycles, timeouts, reallocation events, and the SSD life and
        // normalized-rate attributes are never counters.
        for id in [
            1, 9, 12, 188, 196, 169, 173, 177, 202, 231, 232, 233, 0xA4, 0xA8,
        ] {
            assert_eq!(counter_applies(Ssd, None, id), None, "{id}");
        }
        // A WD profile keeps its HDD-only set; a profile on another kind, or
        // one with no counters of its own (Crucial), scores nothing.
        assert!(counter_applies(Hdd, Some(WD10EZEX_PROFILE), 197).is_some());
        assert_eq!(counter_applies(Hdd, Some(WD10EZEX_PROFILE), 199), None);
        assert_eq!(counter_applies(Ssd, Some(WD10EZEX_PROFILE), 197), None);
        assert_eq!(counter_applies(Ssd, Some("Crucial MX500"), 197), None);
        // ID 5 is ambiguous with a nonzero upper word on either path.
        assert_eq!(scored_counter(Ssd, None, 5, [1, 0, 0, 1, 0, 0]), None);
    }
}
