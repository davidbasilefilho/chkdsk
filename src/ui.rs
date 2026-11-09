//! A janela: lista de discos à esquerda e, à direita, o painel do disco
//! selecionado com a nota em destaque. A janela só desenha cartões já
//! formatados (`present`); nenhuma E/S nem formatação acontece em `view`.

use crate::{
    collector::Update,
    present::{AttributeRow, Body, Card, Health, Tone},
};
use rune::{
    Color, Cursor, Cx, El, Key, KeyEvent, Live, ScrollSpec, Tx, col, oklch, row, text,
    tok::{self, TextStyle},
};
use std::time::{SystemTime, UNIX_EPOCH};

const SIDEBAR_WIDTH: f32 = 272.0;
const SCROLL_STEP: f32 = 56.0;
const DETAIL_KEY: &str = "detalhe";

// Windows type ramp: Caption 12/16, Body 14/20, Subtitle 20/28, Display 72 (semibold, never bold).
const CAPTION: TextStyle = TextStyle::new(12.0, 400.0).lh(16.0);
const CAPTION_STRONG: TextStyle = TextStyle::new(12.0, 600.0).lh(16.0);
const BODY: TextStyle = TextStyle::new(14.0, 400.0).lh(20.0).features("'tnum' 1");
const BODY_STRONG: TextStyle = TextStyle::new(14.0, 600.0).lh(20.0).features("'tnum' 1");
const SUBTITLE: TextStyle = TextStyle::new(20.0, 600.0).lh(28.0);
const SCORE: TextStyle = TextStyle::new(72.0, 600.0).lh(80.0).features("'tnum' 1");

/// A named state for headless captures (`--scene`).
pub type Scene = (&'static str, fn(&mut Dashboard));

/// What the window says when there is no disk to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    Searching,
    NoDisks,
    DiscoveryFailed,
}

impl Notice {
    fn title(self) -> &'static str {
        match self {
            Notice::Searching => "Procurando discos…",
            Notice::NoDisks => "Nenhum disco encontrado",
            Notice::DiscoveryFailed => "Não foi possível localizar os discos",
        }
    }

    fn hint(self) -> Option<&'static str> {
        match self {
            Notice::Searching => None,
            Notice::NoDisks => Some("Conecte um disco; a lista é atualizada a cada 15 segundos."),
            Notice::DiscoveryFailed => Some("Uma nova tentativa acontece em até 15 segundos."),
        }
    }
}

pub struct Dashboard {
    pub cards: Vec<Card>,
    pub selected: usize,
    pub notice: Notice,
    /// Where the detail panel is scrolled, as the viewport reports it.
    scroll: Live<f32>,
}

impl Default for Dashboard {
    fn default() -> Self {
        Self {
            cards: Vec::new(),
            selected: 0,
            notice: Notice::Searching,
            scroll: Live::new(0.0),
        }
    }
}

impl Dashboard {
    /// Take what the collector found. The selected disk stays selected while it
    /// is still there. True when something visible changed.
    pub fn apply(&mut self, update: Update, now: u64) -> bool {
        match update {
            Update::Disks(snapshots) => {
                let cards: Vec<Card> = snapshots
                    .iter()
                    .map(|snapshot| Card::new(snapshot, now))
                    .collect();
                let selected = self
                    .cards
                    .get(self.selected)
                    .and_then(|old| cards.iter().position(|card| card.id == old.id))
                    .unwrap_or(0)
                    .min(cards.len().saturating_sub(1));
                let notice = if cards.is_empty() {
                    Notice::NoDisks
                } else {
                    self.notice
                };
                let changed =
                    cards != self.cards || selected != self.selected || notice != self.notice;
                (self.cards, self.selected, self.notice) = (cards, selected, notice);
                changed
            }
            Update::DiscoveryFailed => {
                let changed = self.notice != Notice::DiscoveryFailed;
                self.notice = Notice::DiscoveryFailed;
                changed
            }
        }
    }

    pub fn select(&mut self, index: usize, tx: &mut Tx) {
        if self.cards.is_empty() {
            return;
        }
        let index = index.min(self.cards.len() - 1);
        if index != self.selected {
            self.selected = index;
            tx.scroll_to_instant(DETAIL_KEY, 0.0, 0.0);
        }
        tx.scroll_into_view(row_key(index));
    }

    fn step(&mut self, forward: bool, tx: &mut Tx) {
        let index = if forward {
            self.selected.saturating_add(1)
        } else {
            self.selected.saturating_sub(1)
        };
        self.select(index, tx);
    }

    fn scroll_by(&self, delta: f32, tx: &mut Tx) {
        tx.scroll_to(DETAIL_KEY, 0.0, (self.scroll.get() + delta).max(0.0));
    }
}

/// The keys of the window: arrows and Tab switch disks, Up and Down scroll the
/// panel, `q` and Esc quit. Page keys are left to rune, which scrolls the panel.
pub fn on_key(state: &mut Dashboard, event: &KeyEvent, tx: &mut Tx) -> bool {
    let mods = event.mods;
    let plain = !(mods.cmd || mods.ctrl || mods.alt || mods.logo);
    match &event.key {
        Key::Escape => tx.quit(),
        Key::Text(text) if plain && text == "q" => tx.quit(),
        Key::Left => state.step(false, tx),
        Key::Right => state.step(true, tx),
        Key::Tab => state.step(!mods.shift, tx),
        Key::Up => state.scroll_by(-SCROLL_STEP, tx),
        Key::Down => state.scroll_by(SCROLL_STEP, tx),
        Key::Text(text) if plain && text == "k" => state.scroll_by(-SCROLL_STEP, tx),
        Key::Text(text) if plain && text == "j" => state.scroll_by(SCROLL_STEP, tx),
        _ => return false,
    }
    true
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |now| now.as_secs())
}

struct Palette {
    theme: tok::Theme,
    accent: Color,
    healthy: Color,
    attention: Color,
    unknown: Color,
}

impl Palette {
    fn new(dark: bool, accent: Color) -> Self {
        // Status colors keep 4.5:1 against the window and list backgrounds in both themes.
        let (healthy, attention, unknown) = if dark {
            (
                oklch(0.80, 0.16, 152.0),
                oklch(0.74, 0.17, 24.0),
                oklch(0.84, 0.14, 88.0),
            )
        } else {
            (
                oklch(0.48, 0.13, 152.0),
                oklch(0.50, 0.19, 27.0),
                oklch(0.50, 0.11, 72.0),
            )
        };
        Self {
            theme: tok::theme(dark),
            accent,
            healthy,
            attention,
            unknown,
        }
    }

    fn tone(&self, tone: Tone) -> Color {
        match tone {
            Tone::Healthy => self.healthy,
            Tone::Attention => self.attention,
            Tone::Unknown => self.unknown,
        }
    }
}

pub fn view(state: &Dashboard, cx: &mut Cx<Dashboard>) -> El {
    let palette = Palette::new(cx.system_dark(), cx.accent());
    let content = match state.cards.get(state.selected) {
        Some(card) => row()
            .grow(1.0)
            .min_h(0.0)
            .child(sidebar(state, &palette))
            .child(detail(card, &palette, &state.scroll)),
        None => empty(state.notice, &palette),
    };
    col()
        .key("janela")
        .w_full()
        .h_full()
        .fill(palette.theme.bg)
        .child(content)
}

fn row_key(index: usize) -> String {
    format!("disco-{index}")
}

fn sidebar(state: &Dashboard, palette: &Palette) -> El {
    let t = palette.theme;
    let mut list = col().gap(4.0).padding(8.0);
    for (index, card) in state.cards.iter().enumerate() {
        let selected = index == state.selected;
        let dot = rune::el()
            .w(8.0)
            .h(8.0)
            .radius(4.0)
            .fill(card.tone().map_or(t.control_off, |tone| palette.tone(tone)));
        let marker = rune::el()
            .w(3.0)
            .h(16.0)
            .radius(1.5)
            .ml(4.0)
            .fill(if selected {
                palette.accent
            } else {
                Color::TRANSPARENT
            });
        let names = col()
            .grow(1.0)
            .min_w(0.0)
            .gap(2.0)
            .child(
                text(card.list_name.clone())
                    .font(if selected { BODY_STRONG } else { BODY })
                    .color(t.text)
                    .truncate(),
            )
            .child(
                text(card.subtitle.clone())
                    .font(CAPTION)
                    .color(t.text_secondary)
                    .truncate(),
            );
        list = list.child(
            row()
                .key(row_key(index))
                .role("tab")
                .label(format!("{}, {}", card.list_name, card.subtitle))
                .checked(selected)
                .h(56.0)
                .px(8.0)
                .gap(8.0)
                .center_y()
                .radius(tok::radius::SM)
                .fill(if selected {
                    t.control_track
                } else {
                    Color::TRANSPARENT
                })
                .hover(t.hover)
                .cursor(Cursor::Pointer)
                .on_press_tx(move |s: &mut Dashboard, tx: &mut Tx| s.select(index, tx))
                .child(marker)
                .child(dot)
                .child(names),
        );
    }
    let hint = col()
        .key("atalhos")
        .gap(2.0)
        .child(
            text("←/→ disco  ·  ↑/↓ rolar")
                .font(CAPTION)
                .color(t.text_secondary),
        )
        .child(
            text("Tab próximo  ·  q sair")
                .font(CAPTION)
                .color(t.text_secondary),
        );
    let panel = col()
        .key("discos")
        .w(SIDEBAR_WIDTH)
        .h_full()
        .fill(t.card)
        .child(
            list.scroll(ScrollSpec {
                stick_bottom: false,
            })
            .grow(1.0)
            .min_h(0.0),
        )
        .child(rune::el().w_full().h(1.0).fill(t.separator))
        .child(col().padding(16.0).child(hint));
    row()
        .h_full()
        .child(panel)
        .child(rune::el().w(1.0).h_full().fill(t.separator))
}

fn empty(notice: Notice, palette: &Palette) -> El {
    let t = palette.theme;
    let mut message = col().gap(8.0).center_y().child(
        text(notice.title())
            .key("aviso")
            .font(SUBTITLE)
            .color(t.text),
    );
    if let Some(hint) = notice.hint() {
        message = message.child(text(hint).font(BODY).color(t.text_secondary));
    }
    col().grow(1.0).min_h(0.0).center().child(message)
}

fn detail(card: &Card, palette: &Palette, scroll: &Live<f32>) -> El {
    let t = palette.theme;
    let mut page = col()
        .w_full()
        .max_w(720.0)
        .padding(32.0)
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
                .mt(4.0),
        );
    page = match &card.body {
        Body::Loading => page.child(
            text("Consultando e interpretando dados…")
                .key("estado")
                .font(BODY)
                .color(t.text_secondary)
                .mt(32.0),
        ),
        Body::Message(message) => page.child(
            text(message.clone())
                .key("estado")
                .font(BODY)
                .color(palette.unknown)
                .wrap()
                .mt(32.0),
        ),
        Body::Health(health) => health_body(page, health, palette),
    };
    let scroll = scroll.clone();
    col()
        .key(DETAIL_KEY)
        .grow(1.0)
        .min_w(0.0)
        .min_h(0.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .on_scroll_offset(move |at| {
            scroll.set(at.offset[1]);
        })
        .child(page)
}

fn health_body(page: El, health: &Health, palette: &Palette) -> El {
    let t = palette.theme;
    let tone = palette.tone(health.tone);
    let pill = row()
        .key("estado")
        .px(12.0)
        .py(4.0)
        .radius(12.0)
        .fill(tone.alpha(0.16))
        .child(text(health.label).font(CAPTION_STRONG).color(tone));
    let mut verdict = col().gap(8.0).ml(20.0).child(pill);
    if health.scored {
        verdict = verdict.child(text("% saúde").font(CAPTION).color(t.text_secondary));
    }
    let hero = row()
        .mt(32.0)
        .center_y()
        .flex_wrap()
        .child(
            text(health.score.clone())
                .key("nota")
                .font(SCORE)
                .color(tone),
        )
        .child(verdict);

    let fact = |key: &'static str, label: &'static str, value: &str, style: TextStyle| {
        row()
            .key(key)
            .gap(16.0)
            .child(text(label).font(BODY).color(t.text_secondary).w(144.0))
            .child(text(value.to_owned()).font(style).color(t.text).wrap())
    };
    let facts = col()
        .mt(32.0)
        .gap(12.0)
        .child(fact("vida", "Vida estimada", &health.lifetime, BODY_STRONG))
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

    let mut page = page.child(hero).child(facts).child(
        text(health.status)
            .key("situacao")
            .font(BODY)
            .color(if health.status_alert {
                palette.attention
            } else {
                t.text_secondary
            })
            .wrap()
            .mt(24.0),
    );
    for warning in &health.warnings {
        page = page.child(
            text(*warning)
                .font(BODY_STRONG)
                .color(palette.attention)
                .wrap()
                .mt(8.0),
        );
    }
    if !health.attributes.is_empty() {
        page = page.child(attributes(&health.attributes, palette));
    }
    page
}

fn attributes(rows: &[AttributeRow], palette: &Palette) -> El {
    let t = palette.theme;
    let line = |cells: [&str; 4], style: TextStyle, color: Color| {
        let cell =
            |content: &str, width: f32| text(content.to_owned()).font(style).color(color).w(width);
        row()
            .gap(16.0)
            .child(cell(cells[0], 40.0))
            .child(cell(cells[1], 56.0))
            .child(cell(cells[2], 56.0))
            .child(text(cells[3].to_owned()).font(style).color(color).grow(1.0))
    };
    let mut table = col()
        .key("atributos")
        .mt(40.0)
        .gap(8.0)
        .child(text("Atributos ATA").font(BODY_STRONG).color(t.text))
        .child(
            text("Valores brutos definidos pelo fabricante, sem unidade atribuída.")
                .font(CAPTION)
                .color(t.text_secondary)
                .wrap(),
        )
        .child(line(["ID", "Atual", "Pior", "Bruto"], CAPTION, t.text_secondary).mt(8.0));
    for attribute in rows {
        table = table.child(line(
            [
                &attribute.id,
                &attribute.current,
                &attribute.worst,
                &attribute.raw,
            ],
            BODY,
            t.text,
        ));
    }
    table
}

#[cfg(test)]
mod tests;
