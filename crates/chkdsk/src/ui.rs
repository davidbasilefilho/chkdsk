//! A janela: a barra de título própria, a lista de discos sobre o fundo e o
//! painel do disco selecionado, elevado e recuado das bordas. A janela só
//! desenha cartões já formatados (`present`); nenhuma E/S nem formatação
//! acontece em `view`.

mod detail;
mod scrollbar;
mod titlebar;

use crate::{
    collector::{DiskSnapshot, Update},
    export::{self, Outcome},
    present::{Card, Tone},
};
use rune::{
    Color, Cursor, Cx, El, Key, KeyEvent, Transition, Tx, Waker, col, el, oklch, row, text,
    tok::{self, TextStyle},
};
use scrollbar::{Bars, Which};
use std::{
    sync::mpsc::{self, Receiver, Sender},
    time::{SystemTime, UNIX_EPOCH},
};

/// Height of the title bar the window draws (`App::titlebar`).
pub const TITLEBAR_HEIGHT: f32 = titlebar::HEIGHT;
const SCROLL_STEP: f32 = 56.0;
const DETAIL_KEY: &str = "detalhe";
const LIST_KEY: &str = "lista";
/// Below this window width the list and the panel give up some room.
const NARROW_BELOW: f32 = 900.0;
/// How long the export confirmation stays in the title bar.
const TOAST_SECONDS: f64 = 5.0;

// Spacing on a 4 pt grid.
const S1: f32 = 4.0;
const S2: f32 = 8.0;
const S3: f32 = 12.0;
const S4: f32 = 16.0;
const S5: f32 = 20.0;
const S6: f32 = 24.0;
const S8: f32 = 32.0;

/// The pill's tint of its own color; low enough for 4.5:1 text whatever the blend mode.
const PILL_TINT: f32 = 0.08;
/// The framed verdict is a larger area, so its tint is lighter: its reasons and advice
/// sit on it and keep 4.5:1.
const FRAME_TINT: f32 = 0.05;

// Windows type ramp: Caption 12/16, Body 14/20, Subtitle 20/28 (semibold, never bold),
// with a larger score in place of Display. Only the wordmark in the title bar is bold.
const CAPTION: TextStyle = TextStyle::new(12.0, 400.0).lh(16.0);
const CAPTION_STRONG: TextStyle = TextStyle::new(12.0, 600.0).lh(16.0).features("'tnum' 1");
const BODY: TextStyle = TextStyle::new(14.0, 400.0).lh(20.0).features("'tnum' 1");
const BODY_STRONG: TextStyle = TextStyle::new(14.0, 600.0).lh(20.0).features("'tnum' 1");
const SUBTITLE: TextStyle = TextStyle::new(20.0, 600.0).lh(28.0);
const SCORE: TextStyle = TextStyle::new(64.0, 600.0).lh(72.0).features("'tnum' 1");

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

/// The export's confirmation in the title bar.
#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    pub outcome: Outcome,
    /// Window clock second at which it goes away; none stays.
    pub until: Option<f64>,
}

/// The export in flight: its thread answers through the channel and wakes the window.
struct Exporting {
    sender: Sender<Outcome>,
    receiver: Receiver<Outcome>,
    waker: Option<Waker>,
    saving: bool,
}

pub struct Dashboard {
    pub cards: Vec<Card>,
    pub selected: usize,
    pub notice: Notice,
    /// What `cards` were made from, for the export.
    snapshots: Vec<DiskSnapshot>,
    /// Avançado is open. Per window: never saved.
    pub advanced: bool,
    /// The window has the keyboard focus; the title bar dims without it.
    focused: bool,
    pub toast: Option<Toast>,
    exporting: Exporting,
    /// The Save dialog; tests put a scripted one here.
    picker: Option<export::Picker>,
    bars: Bars,
}

impl Default for Dashboard {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            cards: Vec::new(),
            selected: 0,
            notice: Notice::Searching,
            snapshots: Vec::new(),
            advanced: false,
            focused: true,
            toast: None,
            exporting: Exporting {
                sender,
                receiver,
                waker: None,
                saving: false,
            },
            picker: export::PICKER,
            bars: Bars::default(),
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
                // A failed discovery is over once a later one lists disks.
                let notice = match (cards.is_empty(), self.notice) {
                    (true, _) => Notice::NoDisks,
                    (false, Notice::DiscoveryFailed) => Notice::Searching,
                    (false, notice) => notice,
                };
                let changed =
                    cards != self.cards || selected != self.selected || notice != self.notice;
                (self.cards, self.selected, self.notice) = (cards, selected, notice);
                self.snapshots = snapshots;
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
    /// A new set of disks lands at once; a reading that changes a disk's score
    /// or status animates.
    pub fn deliver(&mut self, update: Update, now: u64, tx: &mut Tx) -> bool {
        let before = (self.selected, self.selected_id().map(str::to_owned));
        let ids: Vec<String> = self.cards.iter().map(|card| card.id.clone()).collect();
        let changed = self.apply(update, now);
        if self.cards.iter().map(|card| &card.id).ne(ids.iter()) {
            tx.instant();
        } else {
            tx.animated();
            tx.color_motion(tok::motion::CALM);
        }
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
        if self.cards.is_empty() {
            return;
        }
        let index = index.min(self.cards.len() - 1);
        if index != self.selected {
            self.selected = index;
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
        tx.scroll_to(
            DETAIL_KEY,
            0.0,
            (self.bars.detail.offset() + delta).max(0.0),
        );
    }

    /// Open or close Avançado; opening brings its content into view.
    pub fn toggle_advanced(&mut self, tx: &mut Tx) {
        self.advanced = !self.advanced;
        if self.advanced {
            // The header goes to the top, with its content below it.
            tx.scroll_key_to("avancado", S2);
        }
    }

    /// The Export action can run: this platform has the Save dialog, there is
    /// something to write and no dialog is open.
    pub fn can_export(&self) -> bool {
        export::SUPPORTED && !self.snapshots.is_empty() && !self.exporting.saving
    }

    /// Ask where to save and write every disk there. The dialog blocks, so it
    /// and the file run on their own thread; `poll` takes the answer.
    pub fn start_export(&mut self, tx: &mut Tx) {
        if !self.can_export() {
            return;
        }
        self.toast = None;
        let Some(picker) = self.picker else {
            return self.finish_export(Outcome::Unavailable, tx);
        };
        self.exporting.saving = true;
        let snapshots = self.snapshots.clone();
        let (now, owner) = (now_unix(), export::owner_window());
        let sender = self.exporting.sender.clone();
        let waker = self.exporting.waker.clone();
        let spawned = std::thread::Builder::new()
            .name("chkdsk-export".into())
            .spawn(move || {
                let outcome =
                    export::run(&snapshots, now, owner, Some(picker), export::stored_history);
                let _ = sender.send(outcome);
                if let Some(wake) = waker {
                    wake();
                }
            });
        if spawned.is_err() {
            self.finish_export(Outcome::Failed, tx);
        }
    }

    /// The export ended: say how, unless the user only closed the dialog.
    pub fn finish_export(&mut self, outcome: Outcome, tx: &mut Tx) {
        self.exporting.saving = false;
        self.toast = (outcome != Outcome::Cancelled).then(|| {
            let until = tx.clock() + TOAST_SECONDS;
            tx.wake_at(until);
            Toast {
                outcome,
                until: Some(until),
            }
        });
    }
}

/// Runs at the start of every frame: takes an export's answer and ends a
/// confirmation that has stayed long enough.
pub fn poll(state: &mut Dashboard, clock: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    while let Ok(outcome) = state.exporting.receiver.try_recv() {
        state.finish_export(outcome, tx);
        changed = true;
    }
    if state
        .toast
        .as_ref()
        .and_then(|toast| toast.until)
        .is_some_and(|until| clock >= until)
    {
        state.toast = None;
        changed = true;
    }
    changed
}

/// Keeps the waker the export thread calls when its dialog closes.
pub fn setup(state: &mut Dashboard, waker: Waker) {
    state.exporting.waker = Some(waker);
}

/// The window gained or lost the keyboard focus.
pub fn on_focus(state: &mut Dashboard, focused: bool, _: &mut Tx) {
    state.focused = focused;
}

/// The keys of the window: arrows and Tab switch disks (not with Ctrl, Alt or
/// Super held, which belong to the system), Up and Down scroll the panel, `a`
/// opens or closes Avançado, Ctrl+E exports, `q` and Esc quit. Page keys are
/// left to rune, which scrolls the panel.
pub fn on_key(state: &mut Dashboard, event: &KeyEvent, tx: &mut Tx) -> bool {
    let mods = event.mods;
    let plain = !(mods.cmd || mods.ctrl || mods.alt || mods.logo);
    if event.is_command('e') && !mods.alt && !mods.shift {
        state.start_export(tx);
        return true;
    }
    match &event.key {
        Key::Escape => tx.quit(),
        Key::Text(text) if plain && text == "q" => tx.quit(),
        Key::Text(text) if plain && text.eq_ignore_ascii_case("a") => state.toggle_advanced(tx),
        Key::Left if plain => state.step(false, tx),
        Key::Right if plain => state.step(true, tx),
        Key::Tab if plain => state.step(!mods.shift, tx),
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

/// The window's colors. The window background holds the title bar and the list;
/// the panel is the raised surface. Every status color keeps 4.5:1 as text on
/// both, and 3:1 as a dot on the window background.
struct Palette {
    theme: tok::Theme,
    accent: Color,
    healthy: Color,
    caution: Color,
    attention: Color,
}

impl Palette {
    fn new(dark: bool, accent: Color) -> Self {
        let (healthy, caution, attention) = if dark {
            (
                oklch(0.80, 0.16, 152.0),
                oklch(0.82, 0.15, 82.0),
                oklch(0.74, 0.17, 24.0),
            )
        } else {
            (
                oklch(0.45, 0.13, 152.0),
                oklch(0.53, 0.14, 72.0),
                oklch(0.47, 0.20, 25.0),
            )
        };
        Self {
            theme: tok::theme(dark),
            accent,
            healthy,
            caution,
            attention,
        }
    }

    /// No usable data has no hue of its own: it is the quiet text color.
    fn tone(&self, tone: Tone) -> Color {
        match tone {
            Tone::Healthy => self.healthy,
            Tone::Caution => self.caution,
            Tone::Attention => self.attention,
            Tone::Unknown => self.theme.text_secondary,
        }
    }
}

pub fn view(state: &Dashboard, cx: &mut Cx<Dashboard>) -> El {
    let palette = Palette::new(cx.system_dark(), cx.accent());
    let t = palette.theme;
    cx.caption_color(t.text);
    let insets = cx.chrome_insets();
    let narrow = cx
        .layout_with("janela", |rect| rect[2] < NARROW_BELOW)
        .unwrap_or(false);
    let body = match state.cards.get(state.selected) {
        Some(card) => row()
            .grow(1.0)
            .min_h(0.0)
            .child(sidebar(state, &palette, narrow))
            .child(detail::panel(state, card, &palette, narrow)),
        None => row()
            .grow(1.0)
            .min_h(0.0)
            .child(empty(state.notice, &palette)),
    };
    col()
        .key("janela")
        .w_full()
        .h_full()
        .fill(t.bg)
        .child(titlebar::bar(
            state,
            &palette,
            insets.leading,
            insets.trailing,
        ))
        .child(body)
}

fn row_key(id: &str) -> String {
    format!("disco-{id}")
}

fn sidebar(state: &Dashboard, palette: &Palette, narrow: bool) -> El {
    let t = palette.theme;
    let mut list = col().gap(S1).px(S3).pt(S1).pb(S3);
    for (index, card) in state.cards.iter().enumerate() {
        let selected = index == state.selected;
        // A loading disk has no color yet: a ring, as visible as the dots.
        let dot = match card.tone() {
            Some(tone) => el().w(S2).h(S2).radius(S1).fill(palette.tone(tone)),
            None => el().w(S2).h(S2).radius(S1).border(1.5, t.text_secondary),
        };
        // The highlight and the accent marker exist only on the selected row, so
        // each travels to the next one instead of fading out and in.
        let highlight = selected.then(|| {
            el().key("selecao")
                .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                .radius(tok::radius::SM)
                .fill(t.card)
                .matched("selecao")
                .transition(Transition::opacity())
        });
        let marker = el().w(3.0).h(S4).ml(S1).children(selected.then(|| {
            el().w(3.0)
                .h(S4)
                .radius(1.5)
                .fill(palette.accent)
                .matched("marcador")
                .transition(Transition::opacity())
        }));
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
                    .gap(S2)
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
                .role("tab")
                .label(format!("{}, {}, {status}", card.list_name, card.subtitle))
                .checked(selected)
                .h(56.0)
                .px(S2)
                .gap(S2)
                .center_y()
                .radius(tok::radius::SM)
                .hover(t.hover)
                .press(t.press)
                .cursor(Cursor::Pointer)
                .on_press_tx(move |s: &mut Dashboard, tx: &mut Tx| s.select(index, tx))
                .children(highlight)
                .child(marker)
                .child(dot)
                .child(names),
        );
    }
    col()
        .key("discos")
        .w(if narrow { 248.0 } else { 272.0 })
        .h_full()
        .child(
            list.scroll(rune::ScrollSpec {
                stick_bottom: false,
            })
            .key(LIST_KEY)
            .scroll_edge_fade(S4)
            .on_scroll_offset(state.bars.list.follow())
            .grow(1.0)
            .min_h(0.0),
        )
        .child(scrollbar::bar(Which::List, LIST_KEY, state, palette))
}

/// The raised panel's surface, shared by the detail and the empty states.
fn raised(palette: &Palette) -> El {
    let t = palette.theme;
    col()
        .grow(1.0)
        .min_w(0.0)
        .min_h(0.0)
        .mt(S1)
        .mr(S3)
        .mb(S3)
        .fill(t.card)
        .radius(tok::radius::LG)
        .border(1.0, t.card_border)
        .shadow(2.0, 12.0, t.shadow_soft)
        .clip_rounded(tok::radius::LG)
}

fn empty(notice: Notice, palette: &Palette) -> El {
    let t = palette.theme;
    let mut message = col().gap(S2).center_y().child(
        text(notice.title())
            .key("aviso")
            .font(SUBTITLE)
            .color(t.text),
    );
    if let Some(hint) = notice.hint() {
        message = message.child(text(hint).font(BODY).color(t.text_secondary));
    }
    raised(palette).center().ml(S3).child(message)
}

#[cfg(test)]
mod tests;
