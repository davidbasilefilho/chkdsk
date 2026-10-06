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

/// Exact Crucial/Micron families from the upstream smartmontools drive DB,
/// checked against Micron TN-FD-22: SMART 202 raw byte 0 is percent used.
/// The normalized field is `max(100 - raw, 0)` and is not the used percent.
pub(crate) fn profile(model: Option<&str>) -> Option<ModelProfile> {
    let model = model?.trim().to_ascii_uppercase();
    let product = model
        .strip_prefix("CRUCIAL_")
        .or_else(|| model.strip_prefix("CRUCIAL "))
        .unwrap_or(&model)
        .trim_start_matches(|character: char| !character.is_ascii_alphanumeric());
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
    (id == profile.lifetime_used_attribute).then_some(raw[0].min(100))
}

/// Short, user-facing Brazilian Portuguese labels for supported attributes.
pub(crate) fn attribute_name_pt_br(model: Option<&str>, id: u8) -> Option<&'static str> {
    if id == 202 {
        return profile(model).map(|profile| profile.lifetime_remaining_label_pt_br);
    }
    match id {
        5 => Some("Setores realocados"),
        197 => Some("Setores pendentes"),
        198 => Some("Setores incorrigíveis"),
        _ => None,
    }
}

#[allow(dead_code)] // Exposed for the UI's raw-value column when it is rendered.
pub(crate) fn raw_attribute_name_pt_br(model: Option<&str>, id: u8) -> Option<&'static str> {
    if id == 202 {
        return profile(model).map(|profile| profile.lifetime_used_label_pt_br);
    }
    attribute_name_pt_br(model, id)
}

pub(crate) fn raw_u48(raw: [u8; 6]) -> Option<u64> {
    Some(u64::from_le_bytes([
        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], 0, 0,
    ]))
}

pub(crate) fn hdd_sector_signal(id: u8) -> Option<(&'static str, f64)> {
    match id {
        5 => Some(("reallocated sectors", 12.0)),
        197 => Some(("pending sectors", 18.0)),
        198 => Some(("offline uncorrectable sectors", 24.0)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_documented_hdd_sector_signals_are_mapped() {
        assert_eq!(hdd_sector_signal(5), Some(("reallocated sectors", 12.0)));
        assert_eq!(hdd_sector_signal(197), Some(("pending sectors", 18.0)));
        assert_eq!(
            hdd_sector_signal(198),
            Some(("offline uncorrectable sectors", 24.0))
        );
        assert_eq!(hdd_sector_signal(177), None);
        assert_eq!(hdd_sector_signal(202), None);
    }

    #[test]
    fn crucials_supported_202_raw_values_are_used_not_remaining_and_models_are_exact() {
        for model in [
            "CT275MX300SSD1",
            "Crucial CT1050MX300SSD4",
            "CT500MX500SSD1A",
            "CT2000MX500SSD4",
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
        assert_eq!(profile(Some("MacroVIP MV240GB")), None);
        assert_eq!(profile(Some("Samsung SSD 870 EVO")), None);
        assert_eq!(
            endurance_used_percent(Some("CT250MX500SSD1"), 202, [255; 6]),
            Some(100)
        );
        assert_eq!(
            endurance_used_percent(Some("CT250MX500SSD1"), 177, [23; 6]),
            None
        );
    }
}
