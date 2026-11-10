//! The raised panel of the selected disk: name, score, the key facts in an
//! aligned grid, the verdict with its reasons, and Avançado, closed until asked.

use super::{
    BODY, BODY_STRONG, CAPTION, CAPTION_STRONG, DETAIL_KEY, Dashboard, FRAME_TINT, PILL_TINT,
    Palette, S1, S2, S3, S4, S6, S8, SCORE, SUBTITLE, raised, scrollbar::Which,
};
use crate::present::{AttributeRow, Body, Card, Detail, Health, Tone};
use rune::{
    Align, Cursor, Edge, El, Icon, ScrollSpec, Spring, SpringPair, Transition, Tx, col, el, icon,
    rich, row, text,
    text::Span,
    tok::{self, TextStyle},
};

const CONTENT_WIDTH: f32 = 760.0;

static CHEVRON: Icon = Icon {
    name: "chkdsk-chevron",
    view: 16.0,
    body: r#"<path d="M6 3.5l4.5 4.5L6 12.5" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

pub fn panel(state: &Dashboard, card: &Card, palette: &Palette, narrow: bool) -> El {
    let t = palette.theme;
    // The page is keyed by its disk: another disk replaces it with a soft crossfade
    // instead of morphing one disk's numbers into another's.
    let padding = if narrow { S6 } else { S8 };
    let mut page = col()
        .key(format!("painel-{}", card.id))
        .w_full()
        .max_w(CONTENT_WIDTH + 2.0 * padding)
        .padding(padding)
        .transition(Transition::blur_replace())
        .child(
            text(card.title.clone())
                .key("titulo")
                .font(SUBTITLE)
                .color(t.text)
                .wrap(),
        )
        .child(
            text(card.subtitle.clone())
                .font(BODY)
                .color(t.text_secondary)
                .mt(S1),
        );
    page = match &card.body {
        Body::Loading => page.child(
            text("Consultando o disco…")
                .key("estado")
                .font(BODY)
                .color(t.text_secondary)
                .mt(S8),
        ),
        Body::Message(message) => page.child(
            text(message.clone())
                .key("estado")
                .font(BODY)
                .color(t.text)
                .wrap()
                .mt(S8),
        ),
        Body::Health(health) => health_body(page, health, palette, narrow),
    };
    if let Some(advanced) = advanced(state, card, palette, narrow) {
        page = page.child(advanced);
    }
    let viewport = col()
        .key(DETAIL_KEY)
        .grow(1.0)
        .min_w(0.0)
        .min_h(0.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .scroll_edge_fade(S4)
        .on_scroll_offset(state.bars.detail.follow())
        .child(page);
    raised(palette)
        .key("painel")
        .child(viewport)
        .child(super::scrollbar::bar(
            Which::Detail,
            DETAIL_KEY,
            state,
            palette,
        ))
}

/// The score reads `96% saúde` as one line of text, so the percent sign and the
/// caption share the number's baseline. A new number rolls in from below.
fn score(value: u8, palette: &Palette, tone: rune::Color) -> El {
    let t = palette.theme;
    let digits = value.to_string();
    let percent = crate::present::percent(value);
    let content = format!("{percent} saúde");
    let spans = vec![
        Span::new(digits.len(), percent.len())
            .size(28.0)
            .weight(500.0),
        // The space before the caption is the caption's, not the number's 64 pt one.
        Span::new(percent.len(), content.len())
            .size(14.0)
            .weight(400.0)
            .color(t.text_secondary),
    ];
    let roll = Transition::asymmetric(
        Transition::offset(0.0, S4).combined(Transition::opacity()),
        Transition::offset(0.0, -S4).combined(Transition::opacity()),
    )
    .springs(SpringPair::new(
        Spring::new(0.4, 0.1),
        Spring::new(0.25, 0.0),
    ));
    row().key("nota").role("text").label(content.clone()).child(
        rich(content, spans)
            .key(format!("nota-{value}"))
            .font(SCORE)
            .color(tone)
            .transition(roll),
    )
}

fn health_body(page: El, health: &Health, palette: &Palette, narrow: bool) -> El {
    let tone = palette.tone(health.tone);
    let pill = row()
        .key("estado")
        .px(S3)
        .py(S1)
        .radius(S3)
        .fill(tone.alpha(PILL_TINT))
        .child(text(health.label).font(CAPTION_STRONG).color(tone));
    // With no usable evidence there is no number to show, only the pill.
    let hero = row().mt(S6).center_y().gap(S4).flex_wrap();
    let hero = match health.score {
        Some(value) => hero.child(score(value, palette, tone)).child(pill),
        None => hero.child(pill),
    };

    let label_width = if narrow { 144.0 } else { 160.0 };
    let fact = |key: &'static str, label: &'static str, value: &str, style: TextStyle| {
        grid_row(key, label, value, style, label_width, palette)
    };
    let facts = col()
        .mt(S6)
        .child(fact(
            "vida",
            "Vida estimada",
            &health.lifetime,
            if health.lifetime_known {
                BODY_STRONG
            } else {
                BODY
            },
        ))
        .child(fact("desgaste", "Desgaste", &health.wear, BODY))
        .child(fact(
            "temperatura",
            "Temperatura",
            &health.temperature,
            BODY,
        ))
        .child(fact("ligado", "Tempo ligado", &health.power_on, BODY))
        .child(fact(
            "leitura",
            "Última leitura",
            &health.last_reading,
            BODY,
        ));
    page.child(hero)
        .child(facts)
        .child(verdict(health, palette))
}

/// A label and its value on one line: the label in a fixed column, so every
/// value starts at the same x, and both on the same baseline.
fn grid_row(
    key: &str,
    label: &'static str,
    value: &str,
    style: TextStyle,
    label_width: f32,
    palette: &Palette,
) -> El {
    let t = palette.theme;
    // 6 pt above and below centers a 20 pt line in the 32 pt row, and keeps the label
    // level with the first line when a value wraps.
    row()
        .key(key.to_owned())
        .min_h(S8)
        .gap(S4)
        .items_start()
        .py(6.0)
        .child(
            text(label)
                .font(BODY)
                .color(t.text_secondary)
                .w(label_width),
        )
        .child(
            text(value.to_owned())
                .font(style)
                .color(t.text)
                .wrap()
                .grow(1.0)
                .min_w(0.0),
        )
}

/// What the device said and why. A disk that passes with warnings (caution) and
/// one the device reports failing (attention) sit in the same frame but speak in
/// their own hue; a plain status stays a plain line.
fn verdict(health: &Health, palette: &Palette) -> El {
    let t = palette.theme;
    let framed = matches!(health.tone, Tone::Caution | Tone::Attention);
    let status = text(health.status).key("situacao").wrap();
    let status = match health.status_tone {
        Some(Tone::Attention) => status.font(BODY_STRONG).color(palette.attention),
        // A gap the user can close (permissions) is the one thing to read here.
        Some(tone) if tone != Tone::Unknown => status.font(BODY_STRONG).color(palette.tone(tone)),
        Some(_) => status.font(BODY_STRONG).color(t.text),
        None if framed => status.font(BODY).color(t.text),
        None => status.font(BODY).color(t.text_secondary),
    };
    let marker_color = if framed {
        palette.tone(health.tone)
    } else {
        t.text_secondary
    };
    let mut content = col().grow(1.0).min_w(0.0).gap(S2).child(status);
    // The verdict speaks in the tone; each reason is plain text behind a marker,
    // and the advice is the quietest line.
    for (index, warning) in health.warnings.iter().enumerate() {
        content = content.child(
            row()
                .key(format!("motivo-{index}"))
                .gap(S3)
                .child(el().w(6.0).h(6.0).radius(3.0).mt(7.0).fill(marker_color))
                .child(
                    text(warning.clone())
                        .font(BODY)
                        .color(t.text)
                        .wrap()
                        .grow(1.0)
                        .min_w(0.0),
                ),
        );
    }
    if let Some(advice) = health.advice {
        content = content.child(
            text(advice)
                .key("conselho")
                .font(BODY)
                .color(t.text_secondary)
                .wrap(),
        );
    }
    if !framed {
        return content.mt(S6);
    }
    let tone = palette.tone(health.tone);
    // The stripe is the block's own edge, the same bar that marks the open disk.
    row()
        .key("aviso-saude")
        .mt(S6)
        .items_stretch()
        .radius(tok::radius::MD)
        .clip_rounded(tok::radius::MD)
        .fill(tone.alpha(FRAME_TINT))
        .child(el().w(4.0).fill(tone))
        .child(content.padding(S4))
}

/// The accordion: closed until asked, and open it holds the technical details
/// and the ATA attribute table.
fn advanced(state: &Dashboard, card: &Card, palette: &Palette, narrow: bool) -> Option<El> {
    let t = palette.theme;
    let attributes: &[AttributeRow] = match &card.body {
        Body::Health(health) => &health.attributes,
        _ => &[],
    };
    if card.details.is_empty() && attributes.is_empty() {
        return None;
    }
    let open = state.advanced;
    let header = row()
        .key("avancado")
        .role("button")
        .label("Avançado")
        .expanded(open)
        .help("Mostrar detalhes técnicos (A)")
        .h(40.0)
        .px(S2)
        .ml(-S2)
        .gap(S2)
        .center_y()
        .radius(tok::radius::SM)
        .hover(t.hover)
        .press(t.press)
        .cursor(Cursor::Pointer)
        .on_press_tx(|s: &mut Dashboard, tx: &mut Tx| s.toggle_advanced(tx))
        .child(
            icon(&CHEVRON, S4)
                .color(t.text_secondary)
                .rotate(if open {
                    std::f32::consts::FRAC_PI_2
                } else {
                    0.0
                })
                .rotate_motion(Spring::new(0.3, 0.0)),
        )
        .child(text("Avançado").font(BODY_STRONG).color(t.text));
    let mut section = col().mt(S6).child(header);
    if open {
        let mut body = col()
            .key("avancado-conteudo")
            .gap(S6)
            .pt(S2)
            .transition(Transition::reveal(Edge::Bottom).combined(Transition::opacity()));
        if !card.details.is_empty() {
            body = body.child(details(&card.details, palette, narrow));
        }
        if !attributes.is_empty() {
            body = body.child(attribute_table(attributes, palette));
        }
        section = section.child(body);
    }
    Some(section)
}

fn details(details: &[Detail], palette: &Palette, narrow: bool) -> El {
    let width = if narrow { 144.0 } else { 160.0 };
    let mut grid = col().key("tecnico");
    for (index, detail) in details.iter().enumerate() {
        grid = grid.child(grid_row(
            &format!("tecnico-{index}"),
            detail.label,
            &detail.value,
            BODY,
            width,
            palette,
        ));
    }
    grid
}

/// The ATA table: ID, name, then the numbers, right-aligned in fixed columns so
/// their digits line up (tabular figures).
fn attribute_table(rows: &[AttributeRow], palette: &Palette) -> El {
    let t = palette.theme;
    let line = |cells: [&str; 6], style: TextStyle, color| {
        let number = |content: &str, width: f32| {
            text(content.to_owned())
                .font(style)
                .color(color)
                .w(width)
                .align(Align::End)
        };
        // A name that does not fit wraps; the numbers keep their fixed columns.
        row()
            .gap(S2)
            .center_y()
            .child(text(cells[0].to_owned()).font(style).color(color).w(28.0))
            .child(
                text(cells[1].to_owned())
                    .font(style)
                    .color(color)
                    .wrap()
                    .grow(1.0)
                    .min_w(0.0),
            )
            .child(number(cells[2], 40.0))
            .child(number(cells[3], 40.0))
            .child(number(cells[4], 48.0))
            // Extra room before the raw value: its leading zeros must not read as part of the limit.
            .child(number(cells[5], 104.0).ml(S2))
    };
    let mut table = col()
        .key("atributos")
        .child(text("Atributos ATA").font(BODY_STRONG).color(t.text))
        .child(
            text("Valores brutos definidos pelo fabricante, sem unidade atribuída.")
                .font(CAPTION)
                .color(t.text_secondary)
                .wrap()
                .mt(S1),
        )
        .child(
            line(
                ["ID", "Nome", "Atual", "Pior", "Limite", "Bruto"],
                CAPTION,
                t.text_secondary,
            )
            .mt(S3)
            .pb(S2)
            .border_sides([0.0, 0.0, 1.0, 0.0], t.card_border),
        );
    for attribute in rows {
        table = table.child(
            line(
                [
                    &attribute.id,
                    &attribute.name,
                    &attribute.current,
                    &attribute.worst,
                    &attribute.threshold,
                    &attribute.raw,
                ],
                BODY,
                t.text,
            )
            .min_h(S8),
        );
    }
    table
}
