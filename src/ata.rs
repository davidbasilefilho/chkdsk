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
    fn raw_hdd_ids_are_unlabeled_without_a_model_profile() {
        for id in [5, 9, 177, 194, 197, 198, 202] {
            assert_eq!(attribute_name_pt_br(Some("MacroVIP MV240GB"), id), None);
            assert_eq!(raw_attribute_name_pt_br(Some("MacroVIP MV240GB"), id), None);
        }
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
}
