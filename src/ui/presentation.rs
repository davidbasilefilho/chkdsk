//! Flat, opaque presentation. All device interpretation remains in the domain.

use super::*;

/// Five-row ASCII glyphs stay legible without block-font terminal assumptions.
fn glyph(character: char) -> [&'static str; 5] {
    match character {
        '0' => [" ### ", "#   #", "#   #", "#   #", " ### "],
        '1' => ["  #  ", " ##  ", "  #  ", "  #  ", " ### "],
        '2' => [" ### ", "#   #", "   # ", "  #  ", "#####"],
        '3' => ["#### ", "    #", " ### ", "    #", "#### "],
        '4' => ["#   #", "#   #", "#####", "    #", "    #"],
        '5' => ["#####", "#    ", "#### ", "    #", "#### "],
        '6' => [" ### ", "#    ", "#### ", "#   #", " ### "],
        '7' => ["#####", "    #", "   # ", "  #  ", " #   "],
        '8' => [" ### ", "#   #", " ### ", "#   #", " ### "],
        '9' => [" ### ", "#   #", " ####", "    #", " ### "],
        '%' => ["##  #", "## # ", "  #  ", " # ##", "#  ##"],
        'N' => ["#   #", "##  #", "# # #", "#  ##", "#   #"],
        '/' => ["    #", "   # ", "  #  ", " #   ", "#    "],
        'A' => [" ### ", "#   #", "#####", "#   #", "#   #"],
        _ => ["     "; 5],
    }
}

pub(super) fn ascii_score(text: &str) -> Vec<String> {
    let glyphs = text.chars().map(glyph).collect::<Vec<_>>();
    (0..5)
        .map(|row| glyphs.iter().map(|g| g[row]).collect::<Vec<_>>().join(" "))
        .collect()
}

fn centered_line(
    ops: &mut RenderOpIRVec,
    area: HitArea,
    y: usize,
    text: &str,
    color: RgbValue,
    bold: bool,
) {
    let text = clipped(text, area.width.saturating_sub(2).max(1));
    let x = area.x + area.width.saturating_sub(display_width(&text)) / 2;
    paint_background(
        ops,
        x,
        y,
        &text,
        color,
        bold,
        area.width.saturating_sub(x - area.x),
        Some(TOKYO_BG),
    );
}

/// The main disk surface contains only the score and a secondary ETA.
/// Explanations, confidence and device metrics live in the collapsed panels.
pub(super) fn render_disk_summary(
    ops: &mut RenderOpIRVec,
    disk: &DiskView,
    history: &History,
    area: HitArea,
) {
    if !area.visible() {
        return;
    }
    let health = disk
        .telemetry
        .as_ref()
        .map(|t| score_health(effective_disk_kind(&disk.identity, t), t));
    let (text, color) = match health.as_ref() {
        Some(health) if health.outcome != HealthOutcome::Unknown => {
            (format!("{}%", health.score), health_presentation(health).1)
        }
        _ => ("N/A".into(), TOKYO_FG_DARK),
    };
    let kind = disk.telemetry.as_ref().map_or(disk.identity.kind, |t| {
        effective_disk_kind(&disk.identity, t)
    });
    let eta = if matches!(kind, DiskKind::Ssd | DiskKind::Nvme) {
        history
            .estimate_endurance(&disk.identity.id)
            .map(|estimate| {
                format!(
                    "ETA restante  {}",
                    format_endurance_days(estimate.remaining_seconds)
                )
            })
    } else {
        None
    };
    let eta = eta.as_deref().unwrap_or("ETA —");
    let ascii = ascii_score(&text);
    let scale = if area.height >= 13 && display_width(&ascii[0]) * 2 + 2 <= area.width {
        2
    } else {
        1
    };
    let large = area.height >= 8 && display_width(&ascii[0]) + 2 <= area.width;
    let needed = if large { 5 * scale + 3 } else { 2 };
    let top = area.y + area.height.saturating_sub(needed) / 2;
    if large {
        for (row, line) in ascii.iter().enumerate() {
            let expanded = line
                .chars()
                .flat_map(|c| std::iter::repeat_n(c, scale))
                .collect::<String>();
            for repeat in 0..scale {
                centered_line(
                    ops,
                    area,
                    top + row * scale + repeat,
                    &expanded,
                    color,
                    true,
                );
            }
        }
        // The compact label retains screen-reader/plain-text accessibility.
        centered_line(ops, area, top + 5 * scale, &text, color, true);
        centered_line(ops, area, top + 5 * scale + 2, eta, TOKYO_FG_DARK, false);
    } else {
        centered_line(ops, area, top, &text, color, true);
        if top + 1 < area.y + area.height {
            centered_line(ops, area, top + 1, eta, TOKYO_FG_DARK, false);
        }
    }
}

/// All ETA eligibility and health semantics are explained away from the hero.
pub(super) fn disk_detail_lines(
    disk: &DiskView,
    history: &History,
    width: usize,
) -> Vec<NotificationLine> {
    let mut values = Vec::<(String, RgbValue, bool)>::new();
    let telemetry = disk.telemetry.as_ref();
    let kind = telemetry.map_or(disk.identity.kind, |t| {
        effective_disk_kind(&disk.identity, t)
    });
    values.push((disk_label(&disk.identity), TOKYO_BLUE, true));
    let kind_label = match kind {
        DiskKind::Nvme => "NVMe",
        DiskKind::Ssd => "SSD",
        DiskKind::Hdd => "HDD",
        DiskKind::Unknown => "Desconhecido",
    };
    for (label, value) in [
        ("Tipo", kind_label.to_owned()),
        (
            "Interface",
            disk.identity
                .bus
                .clone()
                .unwrap_or_else(|| "indisponível".into()),
        ),
        (
            "Capacidade",
            disk.identity
                .capacity_bytes
                .map(format_bytes)
                .unwrap_or_else(|| "indisponível".into()),
        ),
        (
            "Firmware",
            disk.identity
                .firmware
                .clone()
                .unwrap_or_else(|| "indisponível".into()),
        ),
        (
            "Serial",
            disk.identity
                .serial
                .clone()
                .unwrap_or_else(|| "indisponível".into()),
        ),
    ] {
        values.push((format!("{label}: {value}"), TOKYO_FG, false));
    }
    values.push((String::new(), TOKYO_FG, false));
    if let Some(telemetry) = telemetry {
        let health = score_health(kind, telemetry);
        let (label, color) = health_presentation(&health);
        values.push((label.into(), color, true));
        values.push((
            format!(
                "Confiança da leitura: {}%",
                (health.confidence * 100.0).round() as u32
            ),
            TOKYO_FG_DARK,
            false,
        ));
        values.push((
            "Índice heurístico; não é probabilidade de falha nem percentual de vida restante."
                .into(),
            TOKYO_FG_DARK,
            false,
        ));
        values.push((telemetry_status(telemetry).into(), TOKYO_FG, false));
        for (text, color) in health_detail_messages(&health, telemetry) {
            values.push((text, color, false));
        }
        for (label, value) in [
            (
                "Desgaste usado",
                telemetry
                    .endurance_percent
                    .map(|v| format!("{v:.0}%"))
                    .unwrap_or_else(|| "indisponível".into()),
            ),
            (
                "Temperatura",
                telemetry
                    .temperature_c
                    .map(|v| format!("{v} °C"))
                    .unwrap_or_else(|| "indisponível".into()),
            ),
            (
                "Ligado",
                telemetry
                    .power_on_hours
                    .map(|v| format!("{} h", compact_number(v)))
                    .unwrap_or_else(|| "indisponível".into()),
            ),
        ] {
            values.push((format!("{label}: {value}"), TOKYO_FG, false));
        }
    } else {
        values.push((
            disk.error_message
                .clone()
                .unwrap_or_else(|| "Dados de saúde indisponíveis".into()),
            TOKYO_YELLOW,
            false,
        ));
    }
    if disk.querying || disk.computing {
        values.push((
            "Atualizando telemetria · última leitura mantida".into(),
            TOKYO_CYAN,
            false,
        ));
    }
    values.push((String::new(), TOKYO_FG, false));
    let eta = match kind {
        DiskKind::Hdd => {
            "ETA não aplicável a HDD; não há contador portátil de vida restante.".to_owned()
        }
        DiskKind::Unknown => "ETA indisponível · tipo sem previsão".to_owned(),
        _ => history
            .estimate_endurance(&disk.identity.id)
            .map(|e| {
                format!(
                    "ETA restante  {}",
                    format_endurance_days(e.remaining_seconds)
                )
            })
            .unwrap_or_else(|| {
                "ETA indisponível · requer histórico de desgaste com aumento observado".into()
            }),
    };
    values.push((eta, TOKYO_FG_DARK, false));
    values.push((
        "ETA extrapola desgaste nominal; não prevê falhas nem garante vida útil.".into(),
        TOKYO_FG_DARK,
        false,
    ));
    values
        .into_iter()
        .flat_map(|(text, color, bold)| {
            wrap_text(&text, width)
                .into_iter()
                .map(move |text| NotificationLine {
                    text,
                    foreground: color,
                    bold,
                })
        })
        .collect()
}

/// Paint every panel cell after the main surface; blank cells remain opaque.
pub(super) fn render_panel(ops: &mut RenderOpIRVec, state: &Dashboard, layout: &Layout) {
    let area = layout.panel;
    if !area.visible() {
        return;
    }
    for y in area.y..area.y + area.height {
        paint_background(
            ops,
            area.x,
            y,
            &" ".repeat(area.width),
            TOKYO_FG,
            false,
            area.width,
            Some(TOKYO_BG_DARK1),
        );
    }
    let inner_width = area.width.saturating_sub(2).max(1);
    let title = if state.notifications_open {
        "AVISOS"
    } else {
        "DETALHES DO DISCO"
    };
    paint_background(
        ops,
        area.x + usize::from(area.width > 1),
        area.y + usize::from(area.height > 1),
        title,
        TOKYO_BLUE,
        true,
        inner_width,
        Some(TOKYO_BG_DARK1),
    );
    let (lines, scroll) = if state.notifications_open {
        (
            notification_center_lines(state, inner_width),
            state.notification_scroll,
        )
    } else {
        (
            state
                .disks
                .get(state.selected)
                .map(|d| disk_detail_lines(d, &state.history, inner_width))
                .unwrap_or_default(),
            state.content_scroll,
        )
    };
    for (offset, line) in lines
        .iter()
        .skip(scroll)
        .take(panel_visible_lines(layout))
        .enumerate()
    {
        paint_background(
            ops,
            area.x + usize::from(area.width > 1),
            area.y + 3 + offset,
            &line.text,
            line.foreground,
            line.bold,
            inner_width,
            Some(TOKYO_BG_DARK1),
        );
    }
    // Panel footer controls are painted here last and are the only active
    // controls in the overlay rectangle.
    for control in layout
        .controls
        .iter()
        .filter(|c| area.contains(c.area.x, c.area.y))
    {
        paint_button(
            ops,
            control.area,
            control.label,
            TOKYO_BG_DARK1,
            TOKYO_CYAN,
            true,
        );
    }
}
