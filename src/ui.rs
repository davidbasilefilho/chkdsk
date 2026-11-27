//! A janela: lista de discos à esquerda e, à direita, o painel do disco
//! selecionado com a nota em destaque. A janela só desenha cartões já
//! formatados (`present`); nenhuma E/S nem formatação acontece em `view`.

use crate::{
    collector::Update,
    present::{AttributeRow, Body, Card, Health, Tone},
};
use rune::{
    Align, Color, Cursor, Cx, El, Key, KeyEvent, Live, ScrollSpec, Tx, col, oklch, row, text,
    tok::{self, TextStyle},
};
use std::time::{SystemTime, UNIX_EPOCH};

const SIDEBAR_WIDTH: f32 = 272.0;
const SCROLL_STEP: f32 = 56.0;
const DETAIL_KEY: &str = "detalhe";
/// The pill's tint of its own color; low enough for 4.5:1 text whatever the blend mode.
const PILL_TINT: f32 = 0.08;

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
            Notice::Searching => crate::locale::tr("Procurando discos…", "Searching for drives…"),
            Notice::NoDisks => crate::locale::tr("Nenhum disco encontrado", "No drives found"),
            Notice::DiscoveryFailed => crate::locale::tr(
                "Não foi possível localizar os discos",
                "Could not discover drives",
            ),
        }
    }

    fn hint(self) -> Option<&'static str> {
        match self {
            Notice::Searching => None,
            Notice::NoDisks => Some(crate::locale::tr(
                "Conecte um disco; a lista é atualizada a cada 15 segundos.",
                "Connect a drive. This list refreshes every 15 seconds.",
            )),
            Notice::DiscoveryFailed => Some(crate::locale::tr(
                "Uma nova tentativa acontece em até 15 segundos.",
                "Discovery will retry within 15 seconds.",
            )),
        }
    }
}

pub struct Dashboard {
    snapshots: Vec<crate::collector::DiskSnapshot>,
    export_receiver: Option<std::sync::mpsc::Receiver<Option<crate::export::Outcome>>>,
    export_status: Option<String>,
    pub preferences: crate::settings::Settings,
    pub system_motion_off: Option<bool>,
    pub preferences_path: Option<std::path::PathBuf>,
    settings_open: bool,
    save_failed: bool,
    pub cards: Vec<Card>,
    pub selected: usize,
    advanced: bool,
    menu_open: bool,
    pub notice: Notice,
    /// Where the detail panel is scrolled, as the viewport reports it.
    scroll: Live<f32>,
}

impl Default for Dashboard {
    fn default() -> Self {
        Self {
            snapshots: Vec::new(),
            export_receiver: None,
            export_status: None,
            preferences: Default::default(),
            system_motion_off: None,
            preferences_path: None,
            settings_open: false,
            save_failed: false,
            cards: Vec::new(),
            selected: 0,
            advanced: false,
            menu_open: false,
            notice: Notice::Searching,
            scroll: Live::new(0.0),
        }
    }
}

impl Dashboard {
    pub fn show_settings(&mut self) {
        self.settings_open = true;
        self.menu_open = false;
    }
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
                // A failed discovery is over once a later one lists disks.
                let notice = match (cards.is_empty(), self.notice) {
                    (true, _) => Notice::NoDisks,
                    (false, Notice::DiscoveryFailed) => Notice::Searching,
                    (false, notice) => notice,
                };
                let changed =
                    cards != self.cards || selected != self.selected || notice != self.notice;
                self.snapshots = snapshots;
                (self.cards, self.selected, self.notice) = (cards, selected, notice);
                changed
            }
            // Only the empty window shows the notice; with disks listed it stays as is.
            Update::DiscoveryFailed => {
                let changed = self.cards.is_empty() && self.notice != Notice::DiscoveryFailed;
                if self.cards.is_empty() {
                    self.notice = Notice::DiscoveryFailed;
                }
                changed
            }
        }
    }

    /// `apply` for the window: when the selection moves to another disk because
    /// the selected one vanished, the panel opens at its top, as `select` does.
    /// Whenever the selected row moves, the list scrolls to keep it visible.
    pub fn deliver(&mut self, update: Update, now: u64, tx: &mut Tx) -> bool {
        let before = (self.selected, self.selected_id().map(str::to_owned));
        let changed = self.apply(update, now);
        let id = self.selected_id();
        if id != before.1.as_deref() {
            tx.scroll_to_instant(DETAIL_KEY, 0.0, 0.0);
        }
        if let Some(id) = id
            && (self.selected != before.0 || Some(id) != before.1.as_deref())
        {
            tx.scroll_into_view(row_key(id));
        }
        changed
    }

    fn selected_id(&self) -> Option<&str> {
        self.cards.get(self.selected).map(|card| card.id.as_str())
    }

    pub fn select(&mut self, index: usize, tx: &mut Tx) {
        self.settings_open = false;
        if self.cards.is_empty() {
            return;
        }
        if self.menu_open {
            self.menu_open = false;
            tx.focus("disk-menu", true);
        }
        let index = index.min(self.cards.len() - 1);
        if index != self.selected {
            self.selected = index;
            self.advanced = false;
            tx.scroll_to_instant(DETAIL_KEY, 0.0, 0.0);
        }
        tx.scroll_into_view(row_key(&self.cards[index].id));
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

/// The keys of the window: arrows and Tab switch disks (not with Ctrl, Alt or
/// Super held, which belong to the system), Up and Down scroll the panel, `q`
/// and Esc quit. Page keys are left to rune, which scrolls the panel.
pub fn poll(state: &mut Dashboard, _: f64, tx: &mut Tx) -> bool {
    use crate::{export::Outcome, locale::tr};
    use std::sync::mpsc::TryRecvError;
    let Some(rx) = &state.export_receiver else {
        return false;
    };
    let result = match rx.try_recv() {
        Ok(result) => result,
        Err(TryRecvError::Empty) => {
            tx.wake_after(std::time::Duration::from_millis(150));
            return false;
        }
        Err(TryRecvError::Disconnected) => {
            Some(Outcome::Failed(crate::export::Failure::Interrupted))
        }
    };
    state.export_receiver = None;
    state.export_status = result.map(|result| match result {
        Outcome::Saved { name } => format!("{} {name}", tr("Salvo em", "Saved as")),
        Outcome::Unavailable => tr("Exportação indisponível", "Export unavailable").into(),
        Outcome::Failed(reason) => reason.message().into(),
    });
    true
}

fn start_export(state: &mut Dashboard, tx: &mut Tx) {
    let Some(picker) = crate::export::PICKER else {
        return;
    };
    if state.snapshots.is_empty() || state.export_receiver.is_some() {
        return;
    }
    let owner = crate::export::owner_window();
    let snapshots = state.snapshots.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    state.export_status = None;
    let worker = std::thread::Builder::new()
        .name("disksonar-export".into())
        .spawn(move || {
            let result = crate::export::run_isolated(
                &snapshots,
                now_unix(),
                owner,
                Some(picker),
                crate::export::stored_history,
            );
            let _ = sender.send(result);
        });
    if worker.is_ok() {
        state.export_receiver = Some(receiver);
        tx.wake_after(std::time::Duration::from_millis(150));
    } else {
        state.export_status = Some(crate::export::Failure::Start.message().into());
    }
}

pub fn on_key(state: &mut Dashboard, event: &KeyEvent, tx: &mut Tx) -> bool {
    let mods = event.mods;
    let plain = !(mods.cmd || mods.ctrl || mods.alt || mods.logo);
    match &event.key {
        Key::Text(text) if mods.ctrl && text.eq_ignore_ascii_case("e") => start_export(state, tx),
        Key::Escape if state.menu_open => {
            state.menu_open = false;
            tx.focus("disk-menu", true);
        }
        Key::Escape => tx.quit(),
        Key::Text(text) if plain && text == "q" => tx.quit(),
        Key::Left if plain && !state.settings_open => state.step(false, tx),
        Key::Right if plain && !state.settings_open => state.step(true, tx),

        Key::Up if !state.settings_open => state.scroll_by(-SCROLL_STEP, tx),
        Key::Down if !state.settings_open => state.scroll_by(SCROLL_STEP, tx),
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
    pending: Color,
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
                oklch(0.45, 0.13, 152.0),
                oklch(0.47, 0.19, 27.0),
                oklch(0.46, 0.11, 72.0),
            )
        };
        Self {
            theme: tok::theme(dark),
            accent,
            pending: brand_ink(dark),
            healthy,
            attention,
            unknown,
        }
    }

    fn tone(&self, tone: Tone) -> Color {
        match tone {
            Tone::Healthy => self.healthy,
            Tone::Critical => self.attention,
            Tone::Attention => self.unknown,
            Tone::Unknown => self.theme.text_secondary,
        }
    }
}

// The title uses a fixed blue independent of disk status and OS accent.
fn brand_ink(dark: bool) -> Color {
    if dark {
        oklch(0.82, 0.10, 245.0)
    } else {
        oklch(0.40, 0.12, 245.0)
    }
}
pub fn view(state: &Dashboard, cx: &mut Cx<Dashboard>) -> El {
    let dark = state.preferences.dark(cx.system_dark());
    let reduced = state.preferences.reduced(
        state
            .system_motion_off
            .unwrap_or_else(|| cx.system_reduced_motion()),
    );
    let palette = Palette::new(dark, cx.accent());
    cx.caption_color(palette.theme.text);
    cx.reduced_motion(reduced);
    let compact = cx.layout_size("janela").is_some_and(|size| size[0] < 680.0);
    let menu = row()
        .key("disk-menu")
        .role("button")
        .focusable(true)
        .expanded(state.menu_open)
        .label(crate::locale::tr("Menu de discos", "Drive menu"))
        .padding(8.0)
        .cursor(Cursor::Pointer)
        .on_press_tx(|s: &mut Dashboard, tx: &mut Tx| {
            s.menu_open = !s.menu_open;
            if s.menu_open
                && let Some(id) = s.selected_id()
            {
                tx.focus(row_key(id), true);
            }
        })
        .child(text(crate::locale::tr("☰  Discos", "☰  Drives")).color(palette.theme.text));
    let main = if state.settings_open {
        settings_page(state, &palette)
    } else if let Some(card) = state.cards.get(state.selected) {
        detail(card, &palette, &state.scroll, state.advanced)
    } else {
        empty(state.notice, &palette)
    };
    let content = if compact {
        let panel = if state.menu_open {
            sidebar(state, &palette)
        } else {
            main
        };
        col().grow(1.0).min_h(0.0).child(menu).child(panel)
    } else {
        row()
            .grow(1.0)
            .min_h(0.0)
            .child(sidebar(state, &palette))
            .child(main)
    };
    let frame = if reduced {
        col().instant_changes()
    } else {
        col()
    };
    frame
        .key("janela")
        .w_full()
        .h_full()
        .fill(palette.theme.card)
        .child(
            row()
                .h(40.0)
                .window_drag()
                .center_y()
                .pl(cx.chrome_insets().leading + 16.0)
                .pr(cx.chrome_insets().trailing)
                .child(text("disksonar").font(BODY_STRONG).color(brand_ink(dark)))
                .child(rune::el().grow(1.0))
                .children(export_button(state, &palette)),
        )
        .children(state.export_status.as_ref().map(|message| {
            text(message.clone())
                .font(CAPTION)
                .color(palette.theme.text)
                .padding(6.0)
        }))
        .child(content)
}

fn export_button(state: &Dashboard, palette: &Palette) -> Option<El> {
    if !crate::export::SUPPORTED || state.snapshots.is_empty() {
        return None;
    }
    let label = crate::locale::tr("Exportar", "Export");
    let t = palette.theme;
    Some(
        row()
            .key("export")
            .label(label)
            .role("button")
            .focusable(true)
            .padding(6.0)
            .mr(8.0)
            .radius(6.0)
            .center_y()
            .hover(t.hover)
            .on_press_tx(start_export)
            .child(text(label).font(BODY).color(t.text)),
    )
}

pub fn disk_key(id: &str) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hash = DefaultHasher::new();
    id.hash(&mut hash);
    hash.finish()
}
fn row_key(id: &str) -> String {
    format!("disco-{:016x}", disk_key(id))
}

fn sidebar(state: &Dashboard, palette: &Palette) -> El {
    let t = palette.theme;
    let mut list = col().gap(4.0).padding(8.0);
    for (index, card) in state.cards.iter().enumerate() {
        let selected = !state.settings_open && index == state.selected;
        let dot = rune::el().w(8.0).h(8.0).radius(4.0).fill(
            card.tone()
                .map_or(palette.pending, |tone| palette.tone(tone)),
        );
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
        let status = card.status_label();
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
                row()
                    .gap(8.0)
                    .child(
                        text(card.subtitle.clone())
                            .font(CAPTION)
                            .color(t.text_secondary)
                            .truncate()
                            .grow(1.0)
                            .min_w(0.0),
                    )
                    .child(text(status).font(CAPTION_STRONG).color(t.text_secondary)),
            );
        list = list.child(
            row()
                .key(row_key(&card.id))
                .focusable(true)
                .role("tab")
                .label(format!("{}, {}, {status}", card.list_name, card.subtitle))
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
                .child(names)
                .child(dot.self_start().mt(10.0)),
        );
    }
    if state.cards.is_empty() && state.settings_open {
        list = list.child(
            row()
                .key("dashboard-nav")
                .role("button")
                .focusable(true)
                .label(crate::locale::tr("Discos", "Drives"))
                .padding(12.0)
                .on_press_tx(|s: &mut Dashboard, tx: &mut Tx| {
                    s.settings_open = false;
                    s.menu_open = false;
                    tx.focus("settings-nav", true);
                })
                .child(
                    text(crate::locale::tr("Discos", "Drives"))
                        .font(BODY)
                        .color(t.text),
                ),
        );
    }
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
        .child(
            row()
                .key("settings-nav")
                .role("button")
                .focusable(true)
                .cursor(Cursor::Pointer)
                .padding(12.0)
                .label(crate::locale::tr("Configurações", "Settings"))
                .on_press_tx(|s: &mut Dashboard, tx: &mut Tx| {
                    s.show_settings();
                    tx.focus("theme-system", true);
                })
                .child(
                    text(crate::locale::tr("Configurações", "Settings"))
                        .font(BODY_STRONG)
                        .color(t.text),
                ),
        );
    row()
        .h_full()
        .child(panel)
        .child(rune::el().w(1.0).h_full().fill(t.separator))
}

#[derive(Clone, Copy)]
enum Choice {
    Theme(crate::settings::Theme),
    Motion(crate::settings::Motion),
}
fn settings_page(state: &Dashboard, palette: &Palette) -> El {
    use crate::{
        locale::tr,
        settings::{Motion, Theme},
    };
    let t = palette.theme;
    let option = |key: &'static str, label: &'static str, choice: Choice| {
        let checked = match choice {
            Choice::Theme(v) => state.preferences.theme == v,
            Choice::Motion(v) => state.preferences.motion == v,
        };
        row()
            .key(key)
            .role("radio")
            .checked(checked)
            .label(label)
            .focusable(true)
            .cursor(Cursor::Pointer)
            .px(12.0)
            .py(8.0)
            .radius(6.0)
            .fill(if checked {
                t.control_track
            } else {
                Color::TRANSPARENT
            })
            .hover(t.hover)
            .on_press_tx(move |s: &mut Dashboard, tx: &mut Tx| {
                match choice {
                    Choice::Theme(v) => s.preferences.theme = v,
                    Choice::Motion(v) => {
                        s.preferences.motion = v;
                        tx.settle();
                    }
                }
                s.save_failed = s
                    .preferences_path
                    .as_deref()
                    .is_some_and(|p| s.preferences.save(p).is_err());
            })
            .child(
                text(label)
                    .font(if checked { BODY_STRONG } else { BODY })
                    .color(t.text),
            )
    };
    let group = |label: &'static str| {
        col()
            .role("radiogroup")
            .label(label)
            .mt(16.0)
            .gap(3.0)
            .child(text(label).font(BODY_STRONG).color(t.text))
    };
    let mut page = col()
        .key("settings-page")
        .min_w(0.0)
        .min_h(0.0)
        .grow(1.0)
        .radius(12.0)
        .mr(8.0)
        .mb(8.0)
        .padding(18.0)
        .fill(t.bg)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .child(
            text(tr("Configurações", "Settings"))
                .font(SUBTITLE)
                .color(t.text),
        )
        .child(
            group(tr("Tema", "Theme"))
                .child(option(
                    "theme-system",
                    tr("Sistema", "System"),
                    Choice::Theme(Theme::System),
                ))
                .child(option(
                    "theme-light",
                    tr("Claro", "Light"),
                    Choice::Theme(Theme::Light),
                ))
                .child(option(
                    "theme-dark",
                    tr("Escuro", "Dark"),
                    Choice::Theme(Theme::Dark),
                )),
        )
        .child(
            group(tr("Animações", "Animations"))
                .child(option(
                    "motion-system",
                    tr("Sistema", "System"),
                    Choice::Motion(Motion::System),
                ))
                .child(option(
                    "motion-on",
                    tr("Ativadas", "On"),
                    Choice::Motion(Motion::On),
                ))
                .child(option(
                    "motion-off",
                    tr("Desativadas", "Off"),
                    Choice::Motion(Motion::Off),
                )),
        );
    if state.save_failed {
        page = page.child(
            text(tr(
                "Não foi possível salvar as preferências.",
                "Could not save preferences.",
            ))
            .font(BODY)
            .color(t.text),
        );
    }
    page
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

fn detail(card: &Card, palette: &Palette, scroll: &Live<f32>, advanced: bool) -> El {
    let t = palette.theme;
    let mut page = col()
        .w_full()
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
            text(crate::locale::tr("Consultando o disco…", "Reading drive…"))
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
        Body::Health(health) => health_body(page, health, palette, advanced),
    };
    let scroll = scroll.clone();
    col()
        .radius(12.0)
        .mr(8.0)
        .mb(8.0)
        .fill(t.bg)
        .key(DETAIL_KEY)
        .ml(8.0)
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

fn health_body(page: El, health: &Health, palette: &Palette, advanced: bool) -> El {
    let t = palette.theme;
    let tone = palette.tone(health.tone);
    let pill = row()
        .key("estado")
        .px(12.0)
        .py(4.0)
        .radius(12.0)
        .fill(tone.alpha(PILL_TINT))
        .child(text(health.label).font(CAPTION_STRONG).color(tone));
    // With no usable evidence there is no number to show, only the pill.
    let hero = col().mt(24.0).child(pill.self_start());
    let hero = match &health.score {
        Some(value) => hero
            .child(text(value.clone()).key("nota").font(SCORE).color(tone))
            .child(
                text(crate::locale::tr("de saúde", "health"))
                    .font(CAPTION)
                    .color(t.text_secondary),
            )
            .child(
                text(crate::locale::tr(
                    "Resistência nominal restante do SSD",
                    "Remaining rated SSD endurance",
                ))
                .font(CAPTION)
                .color(t.text_secondary),
            ),
        None => hero,
    };

    let fact = |key: &'static str, label: &'static str, value: &str, style: TextStyle| {
        col()
            .key(key)
            .w(220.0)
            .grow(1.0)
            .padding(12.0)
            .gap(4.0)
            .radius(8.0)
            .fill(t.card)
            .child(text(label).font(CAPTION).color(t.text_secondary))
            .child(text(value.to_owned()).font(style).color(t.text).wrap())
    };
    let facts = row()
        .flex_wrap()
        .mt(32.0)
        .gap(12.0)
        .child(fact(
            "vida",
            crate::locale::tr("Vida estimada", "Estimated endurance"),
            &health.lifetime,
            if health.lifetime_known {
                BODY_STRONG
            } else {
                BODY
            },
        ))
        .child(fact(
            "desgaste",
            crate::locale::tr("Desgaste", "Wear used"),
            &health.wear,
            BODY,
        ))
        .child(fact(
            "temperatura",
            crate::locale::tr("Temperatura", "Temperature"),
            &health.temperature,
            BODY,
        ))
        .child(fact(
            "ligado",
            crate::locale::tr("Tempo ligado", "Power-on time"),
            &health.power_on,
            BODY,
        ))
        .child(fact(
            "leitura",
            crate::locale::tr("Última leitura", "Last reading"),
            &health.last_reading,
            BODY,
        ));

    let status = text(health.status).key("situacao").wrap().mt(24.0);
    let status = match health.status_tone {
        Some(Tone::Attention) => status.font(BODY_STRONG).color(palette.attention),
        Some(tone) => status.font(BODY).color(palette.tone(tone)),
        None => status.font(BODY).color(t.text_secondary),
    };
    let mut page = page.child(hero).child(facts).child(status);
    for warning in &health.warnings {
        page = page.child(
            text(*warning)
                .font(BODY_STRONG)
                .color(palette.attention)
                .wrap()
                .mt(8.0),
        );
    }
    if !health.attributes.is_empty() || !health.details.is_empty() {
        page = page.child(
            row()
                .key("advanced")
                .role("button")
                .focusable(true)
                .expanded(advanced)
                .label(crate::locale::tr(
                    "Informações avançadas",
                    "Advanced information",
                ))
                .cursor(Cursor::Pointer)
                .padding(12.0)
                .on_press_tx(|state: &mut Dashboard, _: &mut Tx| state.advanced = !state.advanced)
                .child(
                    text(if advanced {
                        crate::locale::tr("▾  Informações avançadas", "▾  Advanced information")
                    } else {
                        crate::locale::tr("▸  Informações avançadas", "▸  Advanced information")
                    })
                    .font(BODY_STRONG)
                    .color(t.text),
                ),
        );
        if advanced {
            for (name, value) in &health.details {
                page = page.child(
                    text(format!("{name} · {value}"))
                        .font(BODY)
                        .color(t.text)
                        .wrap(),
                );
            }
            if !health.attributes.is_empty() {
                page = page.child(attributes(&health.attributes, palette));
            }
        }
    }
    page
}

fn attributes(rows: &[AttributeRow], palette: &Palette) -> El {
    let t = palette.theme;
    let line = |cells: [&str; 5], style: TextStyle, color: Color| {
        let cell = |content: &str, width: f32, align: Align| {
            text(content.to_owned())
                .font(style)
                .color(color)
                .w(width)
                .align(align)
        };
        row()
            .gap(8.0)
            .child(cell(cells[0], 28.0, Align::Start))
            .child(cell(cells[1], 44.0, Align::End))
            .child(cell(cells[2], 32.0, Align::End))
            .child(cell(cells[3], 48.0, Align::End))
            .child(text(cells[4].to_owned()).font(style).color(color).grow(1.0))
    };
    let mut table = col()
        .key("atributos")
        .mt(40.0)
        .gap(8.0)
        .child(
            text(crate::locale::tr("Atributos ATA", "ATA attributes"))
                .font(BODY_STRONG)
                .color(t.text),
        )
        .child(
            text(crate::locale::tr(
                "Valores brutos definidos pelo fabricante, sem unidade atribuída.",
                "Vendor-defined raw values. Units are unknown.",
            ))
            .font(CAPTION)
            .color(t.text_secondary)
            .wrap(),
        )
        .child(
            line(
                [
                    "ID",
                    crate::locale::tr("Atual", "Current"),
                    crate::locale::tr("Pior", "Worst"),
                    crate::locale::tr("Limite", "Limit"),
                    crate::locale::tr("Bruto", "Raw"),
                ],
                CAPTION,
                t.text_secondary,
            )
            .mt(8.0),
        );
    for attribute in rows {
        table = table.child(line(
            [
                &attribute.id,
                &attribute.current,
                &attribute.worst,
                &attribute.threshold,
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
