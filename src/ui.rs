//! Interface terminal em português brasileiro. A tradução fica nesta fronteira:
//! detalhes e mensagens do sistema operacional nunca são exibidos diretamente.

use crate::{
    backend::{self, QueryErrorState},
    health::{HealthOutcome, score_health},
    history::{HealthSample, History},
    model::{DiskIdentity, DiskKind, QueryState, Telemetry},
};
use r3bl_tui::{
    App, BoxedSafeApp, Button, CommonResult, ComponentRegistryMap, EventPropagation, GlobalData,
    HasFocus, InputEvent, Key, KeyPress, MouseInputKind, RenderOpCommon, RenderOpIR, RenderOpIRVec,
    RenderPipeline, RgbValue, SpecialKey, TerminalWindowMainThreadSignal, TuiColor, TuiStyle,
    ZOrder,
};
use std::{
    collections::{HashMap, VecDeque},
    fmt::{Display, Formatter},
    io::Write,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::mpsc::{Sender, UnboundedReceiver, UnboundedSender},
    task::JoinSet,
    time::{Duration, MissedTickBehavior},
};

const MOCHA_BASE: RgbValue = RgbValue {
    red: 30,
    green: 30,
    blue: 46,
};
const MOCHA_MANTLE: RgbValue = RgbValue {
    red: 24,
    green: 24,
    blue: 37,
};
const MOCHA_SURFACE0: RgbValue = RgbValue {
    red: 49,
    green: 50,
    blue: 68,
};
const MOCHA_SURFACE1: RgbValue = RgbValue {
    red: 69,
    green: 71,
    blue: 90,
};
const MOCHA_SURFACE2: RgbValue = RgbValue {
    red: 88,
    green: 91,
    blue: 112,
};
const MOCHA_TEXT: RgbValue = RgbValue {
    red: 205,
    green: 214,
    blue: 244,
};
const MOCHA_MUTED: RgbValue = RgbValue {
    red: 166,
    green: 173,
    blue: 200,
};
const MOCHA_LAVENDER: RgbValue = RgbValue {
    red: 180,
    green: 190,
    blue: 254,
};
const MOCHA_GREEN: RgbValue = RgbValue {
    red: 166,
    green: 227,
    blue: 161,
};
const MOCHA_RED: RgbValue = RgbValue {
    red: 243,
    green: 139,
    blue: 168,
};
const MOCHA_YELLOW: RgbValue = RgbValue {
    red: 249,
    green: 226,
    blue: 175,
};
const MOCHA_PEACH: RgbValue = RgbValue {
    red: 250,
    green: 179,
    blue: 135,
};

#[derive(Clone, Debug, PartialEq)]
pub struct DiskView {
    pub identity: DiskIdentity,
    pub telemetry: Option<Telemetry>,
    pub querying: bool,
    pub computing: bool,
    pending_telemetry: Option<Telemetry>,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dashboard {
    pub disks: Vec<DiskView>,
    pub selected: usize,
    pub scroll: usize,
    pub hovered_tab: Option<usize>,
    pub content_scroll: usize,
    pub notice: Option<String>,
    pub history: History,
    pub generation: u64,
    pub frame: usize,
}

impl Display for Dashboard {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Painel[discos={}, selecionado={}]",
            self.disks.len(),
            self.selected
        )
    }
}

#[derive(Clone, Debug, Default)]
#[allow(clippy::large_enum_variant)]
pub enum AppSignal {
    #[default]
    Noop,
    Discovered {
        generation: u64,
        result: Result<Vec<DiskIdentity>, String>,
    },
    QueryFinished {
        generation: u64,
        disk_id: String,
        result: Result<Telemetry, QueryErrorState>,
    },
    ComputeFinished {
        generation: u64,
        disk_id: String,
    },
    Frame,
    HistoryLoaded(History),
}

#[derive(Default)]
pub struct DashboardApp {
    started: bool,
    history_writer: Option<UnboundedSender<History>>,
}

impl DashboardApp {
    pub fn new_boxed() -> BoxedSafeApp<Dashboard, AppSignal> {
        Box::new(Self::default())
    }
}

impl App for DashboardApp {
    type S = Dashboard;
    type AS = AppSignal;

    fn app_init(
        &mut self,
        _components: &mut ComponentRegistryMap<Dashboard, AppSignal>,
        _focus: &mut HasFocus,
    ) {
    }

    fn app_handle_input_event(
        &mut self,
        event: InputEvent,
        global: &mut GlobalData<Dashboard, AppSignal>,
        _components: &mut ComponentRegistryMap<Dashboard, AppSignal>,
        _focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        let state = &mut global.state;
        match event {
            InputEvent::Keyboard(KeyPress::Plain { key }) => match key {
                Key::Character('q') | Key::SpecialKey(SpecialKey::Esc) => {
                    return Ok(EventPropagation::ExitMainEventLoop);
                }
                Key::SpecialKey(SpecialKey::Left) | Key::SpecialKey(SpecialKey::BackTab) => {
                    move_selection(state, false, 1);
                }
                Key::SpecialKey(SpecialKey::Right) | Key::SpecialKey(SpecialKey::Tab) => {
                    move_selection(state, true, 1);
                }
                Key::SpecialKey(SpecialKey::Up) | Key::Character('k') => {
                    state.content_scroll = state.content_scroll.saturating_sub(1);
                }
                Key::SpecialKey(SpecialKey::Down) | Key::Character('j') => {
                    state.content_scroll = state.content_scroll.saturating_add(1);
                }
                Key::SpecialKey(SpecialKey::PageUp) => {
                    state.content_scroll = state.content_scroll.saturating_sub(5);
                }
                Key::SpecialKey(SpecialKey::PageDown) => {
                    state.content_scroll = state.content_scroll.saturating_add(5);
                }
                _ => return Ok(EventPropagation::Propagate),
            },
            InputEvent::Mouse(mouse) => {
                let x: usize = (*mouse.pos.col_index).into();
                let y: usize = (*mouse.pos.row_index).into();
                let layout = Layout::new(
                    (*global.window_size.col_width).into(),
                    (*global.window_size.row_height).into(),
                    state.disks.len(),
                    state.scroll,
                );
                if mouse.kind == MouseInputKind::MouseMove {
                    let hovered_tab = layout.tab_at(x, y);
                    if state.hovered_tab != hovered_tab {
                        state.hovered_tab = hovered_tab;
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    return Ok(EventPropagation::Propagate);
                }
                match mouse.kind {
                    MouseInputKind::MouseDown(Button::Left) => {
                        if layout.quit.contains(x, y) {
                            return Ok(EventPropagation::ExitMainEventLoop);
                        }
                        if let Some(index) = layout.tab_at(x, y) {
                            state.selected = index;
                            state.hovered_tab = Some(index);
                            state.content_scroll = 0;
                            ensure_selected_visible(
                                state,
                                (*global.window_size.col_width).into(),
                                (*global.window_size.row_height).into(),
                            );
                        }
                    }
                    MouseInputKind::ScrollUp
                        if layout.tabs.iter().any(|tab| tab.contains(x, y)) =>
                    {
                        move_selection(state, false, 1);
                    }
                    MouseInputKind::ScrollDown
                        if layout.tabs.iter().any(|tab| tab.contains(x, y)) =>
                    {
                        move_selection(state, true, 1);
                    }
                    MouseInputKind::ScrollUp => {
                        state.content_scroll = state.content_scroll.saturating_sub(1)
                    }
                    MouseInputKind::ScrollDown => {
                        state.content_scroll = state.content_scroll.saturating_add(1)
                    }
                    _ => return Ok(EventPropagation::Propagate),
                }
            }
            _ => return Ok(EventPropagation::Propagate),
        }
        ensure_selected_visible(
            state,
            (*global.window_size.col_width).into(),
            (*global.window_size.row_height).into(),
        );
        Ok(EventPropagation::ConsumedRender)
    }

    fn app_handle_signal(
        &mut self,
        signal: &AppSignal,
        global: &mut GlobalData<Dashboard, AppSignal>,
        _components: &mut ComponentRegistryMap<Dashboard, AppSignal>,
        _focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        let state = &mut global.state;
        match signal {
            AppSignal::Discovered {
                generation,
                result: Ok(disks),
            } => {
                if *generation < state.generation {
                    return Ok(EventPropagation::Consumed);
                }
                state.generation = *generation;
                let old_id = state
                    .disks
                    .get(state.selected)
                    .map(|disk| disk.identity.id.clone());
                let previous = std::mem::take(&mut state.disks);
                state.disks = disks
                    .iter()
                    .cloned()
                    .map(|identity| {
                        let telemetry = previous
                            .iter()
                            .find(|disk| disk.identity == identity)
                            .and_then(|disk| disk.telemetry.clone());
                        DiskView {
                            identity,
                            telemetry,
                            querying: true,
                            computing: false,
                            pending_telemetry: None,
                            error_message: None,
                        }
                    })
                    .collect();
                state.selected = old_id
                    .and_then(|id| state.disks.iter().position(|disk| disk.identity.id == id))
                    .unwrap_or(0)
                    .min(state.disks.len().saturating_sub(1));
                state.notice = if state.disks.is_empty() {
                    Some("Nenhum disco encontrado".into())
                } else {
                    None
                };
            }
            AppSignal::Discovered {
                generation,
                result: Err(_),
            } => {
                if *generation < state.generation {
                    return Ok(EventPropagation::Consumed);
                }
                state.notice = Some("Não foi possível localizar os discos".into())
            }
            AppSignal::QueryFinished {
                generation,
                disk_id,
                result,
            } => {
                if *generation != state.generation {
                    return Ok(EventPropagation::Consumed);
                }
                if let Some(disk) = state
                    .disks
                    .iter_mut()
                    .find(|disk| disk.identity.id == *disk_id)
                {
                    if let Ok(telemetry) = result {
                        disk.querying = false;
                        disk.computing = true;
                        disk.pending_telemetry = Some(telemetry.clone());
                        disk.error_message = None;
                        let sender = global.main_thread_channel_sender.clone();
                        let disk_id = disk_id.clone();
                        let generation = *generation;
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(120)).await;
                            let _ = sender
                                .send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                                    AppSignal::ComputeFinished {
                                        generation,
                                        disk_id,
                                    },
                                ))
                                .await;
                        });
                    } else {
                        disk.querying = false;
                        disk.computing = false;
                        disk.telemetry = None;
                        disk.error_message = Some(
                            match result {
                                Err(QueryErrorState::Permission) => {
                                    "Permissão insuficiente para consultar o dispositivo"
                                }
                                Err(QueryErrorState::Unsupported) => {
                                    "Leitura de saúde não compatível com o dispositivo"
                                }
                                Err(QueryErrorState::Transient | QueryErrorState::Partial) => {
                                    "Não foi possível consultar os dados de saúde"
                                }
                                Ok(_) => unreachable!(),
                            }
                            .into(),
                        );
                    }
                }
            }
            AppSignal::ComputeFinished {
                generation,
                disk_id,
            } => {
                if *generation != state.generation {
                    return Ok(EventPropagation::Consumed);
                }
                if let Some(disk) = state
                    .disks
                    .iter_mut()
                    .find(|disk| disk.identity.id == *disk_id)
                {
                    disk.computing = false;
                    if let Some(telemetry) = disk.pending_telemetry.take() {
                        let score = score_health(disk.identity.kind, &telemetry);
                        if let Some(timestamp_unix_secs) = next_timestamp(&state.history, disk_id) {
                            let sample = HealthSample {
                                disk_id: disk.identity.id.clone(),
                                timestamp_unix_secs,
                                kind: disk.identity.kind,
                                endurance_percent: telemetry.endurance_percent,
                                power_on_hours: telemetry.power_on_hours,
                                score: (score.outcome != HealthOutcome::Unknown)
                                    .then_some(score.score),
                            };
                            if state.history.record(sample).is_ok()
                                && let Some(writer) = &self.history_writer
                            {
                                let _ = writer.send(state.history.clone());
                            }
                        }
                        disk.telemetry = Some(telemetry);
                    }
                }
            }
            AppSignal::HistoryLoaded(history) => state.history = history.clone(),
            AppSignal::Frame => {
                if !state
                    .disks
                    .iter()
                    .any(|disk| disk.querying || disk.computing)
                {
                    return Ok(EventPropagation::Consumed);
                }
                state.frame = state.frame.wrapping_add(1);
            }
            AppSignal::Noop => return Ok(EventPropagation::Consumed),
        }
        Ok(EventPropagation::ConsumedRender)
    }

    fn app_render(
        &mut self,
        global: &mut GlobalData<Dashboard, AppSignal>,
        _components: &mut ComponentRegistryMap<Dashboard, AppSignal>,
        _focus: &mut HasFocus,
    ) -> CommonResult<RenderPipeline> {
        if !self.started {
            self.started = true;
            self.history_writer = Some(start_history_writer());
            start_background(global.main_thread_channel_sender.clone());
        }
        let state = &global.state;
        let width: usize = (*global.window_size.col_width).into();
        let height: usize = (*global.window_size.row_height).into();
        let layout = Layout::new(width, height, state.disks.len(), state.scroll);
        let mut pipeline = RenderPipeline::default();
        let mut ops = RenderOpIRVec::new();
        ops += RenderOpCommon::SetBgColor(TuiColor::Rgb(MOCHA_BASE));
        ops += RenderOpCommon::SetFgColor(TuiColor::Rgb(MOCHA_TEXT));
        ops += RenderOpCommon::ClearScreen;
        if width == 0 || height == 0 {
            pipeline.push(ZOrder::Normal, ops);
            return Ok(pipeline);
        }

        for y in 0..height {
            paint_background(
                &mut ops,
                0,
                y,
                &" ".repeat(width),
                MOCHA_TEXT,
                false,
                width,
                Some(MOCHA_BASE),
            );
        }
        fill_row(&mut ops, 0, width, MOCHA_MANTLE);
        let title = if width >= 15 {
            format!(" chkdsk v{}", env!("CARGO_PKG_VERSION"))
        } else {
            " chkdsk".to_owned()
        };
        paint_background(
            &mut ops,
            0,
            0,
            &title,
            MOCHA_LAVENDER,
            true,
            width.saturating_sub(layout.quit.width),
            Some(MOCHA_MANTLE),
        );
        if layout.quit.visible() {
            paint_background(
                &mut ops,
                layout.quit.x,
                0,
                if layout.quit.width >= 7 {
                    "[ Sair]"
                } else if layout.quit.width >= 4 {
                    "Sair"
                } else {
                    "×"
                },
                MOCHA_RED,
                true,
                width.saturating_sub(layout.quit.x),
                Some(MOCHA_MANTLE),
            );
        }
        if height > 1 {
            fill_row(&mut ops, 1, width, MOCHA_SURFACE0);
        }
        for (position, area) in layout.tabs.iter().enumerate() {
            let index = layout.tab_indices[position];
            let disk = &state.disks[index];
            let mark = if index == state.selected { "> " } else { "  " };
            let name = disk
                .identity
                .model
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("Disco");
            let physical = format!("#{}", disk.identity.index);
            let model_width = area.width.saturating_sub(mark.len() + physical.len() + 1);
            let model = clipped(name, model_width);
            let label = pad_to_columns(&format!("{mark}{model} {physical}"), area.width);
            let (fg, bg) = if index == state.selected {
                (MOCHA_BASE, MOCHA_LAVENDER)
            } else if state.hovered_tab == Some(index) {
                (MOCHA_TEXT, MOCHA_SURFACE2)
            } else {
                (MOCHA_MUTED, MOCHA_SURFACE1)
            };
            paint_background(
                &mut ops,
                area.x,
                area.y,
                &label,
                fg,
                index == state.selected || state.hovered_tab == Some(index),
                area.width,
                Some(bg),
            );
        }
        if height > layout.body_y {
            if let Some(disk) = state.disks.get(state.selected) {
                render_disk(
                    &mut ops,
                    disk,
                    &state.history,
                    layout.body_y,
                    width,
                    height - layout.body_y - usize::from(height >= 6),
                    state.content_scroll,
                    state.frame,
                );
            } else {
                paint_background(
                    &mut ops,
                    width.min(2).saturating_sub(1),
                    layout.body_y,
                    state.notice.as_deref().unwrap_or("Procurando discos…"),
                    MOCHA_MUTED,
                    false,
                    width.saturating_sub(2),
                    Some(MOCHA_SURFACE0),
                );
            }
        }
        if height >= 6 {
            fill_row(&mut ops, height - 1, width, MOCHA_MANTLE);
            let controls = if width >= 40 {
                "←/→ discos  ↑/↓ detalhes  Tab próximo  q sair"
            } else if width >= 27 {
                "←/→ disco  ↑/↓  Tab  q sair"
            } else if width >= 16 {
                "←/→  ↑/↓  Tab  q"
            } else {
                "q sair"
            };
            paint_background(
                &mut ops,
                0,
                height - 1,
                controls,
                MOCHA_MUTED,
                false,
                width,
                Some(MOCHA_MANTLE),
            );
        }
        pipeline.push(ZOrder::Normal, ops);
        Ok(pipeline)
    }
}

fn move_selection(state: &mut Dashboard, forward: bool, amount: usize) {
    if state.disks.is_empty() {
        return;
    }
    state.selected = if forward {
        state
            .selected
            .saturating_add(amount)
            .min(state.disks.len() - 1)
    } else {
        state.selected.saturating_sub(amount)
    };
    state.content_scroll = 0;
}

fn ensure_selected_visible(state: &mut Dashboard, width: usize, height: usize) {
    let layout = Layout::new(width, height, state.disks.len(), state.scroll);
    if layout.tab_indices.contains(&state.selected) {
        return;
    }
    if let Some(last) = layout.tab_indices.last().copied() {
        if state.selected > last {
            state.scroll = state
                .selected
                .saturating_sub(layout.tab_indices.len().saturating_sub(1));
        } else {
            state.scroll = state.selected;
        }
    } else {
        state.scroll = state.selected;
    }
}

const MAX_CONCURRENT_DISK_QUERIES: usize = 4;

#[derive(Default)]
struct QueryScheduler {
    generation: u64,
    disks: Vec<DiskIdentity>,
    pending: VecDeque<DiskIdentity>,
    in_flight: HashMap<String, u64>,
}

impl QueryScheduler {
    fn update_disks(&mut self, mut disks: Vec<DiskIdentity>) {
        disks.sort_by(|left, right| left.id.cmp(&right.id));
        if self.disks != disks {
            self.generation = self.generation.saturating_add(1);
            self.disks = disks;
        }
        self.pending = self
            .disks
            .iter()
            .filter(|disk| !self.in_flight.contains_key(&disk.id))
            .cloned()
            .collect();
    }

    fn begin_next(&mut self) -> Option<(u64, DiskIdentity)> {
        if self.in_flight.len() >= MAX_CONCURRENT_DISK_QUERIES {
            return None;
        }
        while let Some(identity) = self.pending.pop_front() {
            if self.in_flight.contains_key(&identity.id) {
                continue;
            }
            self.in_flight.insert(identity.id.clone(), self.generation);
            return Some((self.generation, identity));
        }
        None
    }

    /// Retire the in-flight slot, accept only the current topology, and requeue
    /// a still-present disk if its result belonged to an older topology.
    fn finish(&mut self, generation: u64, identity: &DiskIdentity) -> bool {
        if self.in_flight.remove(&identity.id) != Some(generation) {
            return false;
        }
        let current = self.disks.iter().find(|disk| disk.id == identity.id);
        if generation == self.generation && current == Some(identity) {
            return true;
        }
        if let Some(current) = current
            && !self.pending.iter().any(|disk| disk.id == current.id)
        {
            self.pending.push_back(current.clone());
        }
        false
    }
}

fn start_background(sender: Sender<TerminalWindowMainThreadSignal<AppSignal>>) {
    tokio::spawn(async move {
        let animation_sender = sender.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(120));
            loop {
                ticker.tick().await;
                if animation_sender
                    .send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                        AppSignal::Frame,
                    ))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let path = history_path();
        if let Ok(history) = tokio::task::spawn_blocking(move || {
            std::fs::read(path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<History>(&bytes).ok())
                .unwrap_or_default()
        })
        .await
        {
            let _ = sender
                .send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                    AppSignal::HistoryLoaded(history),
                ))
                .await;
        }

        let mut scheduler = QueryScheduler::default();
        let mut jobs = JoinSet::<(u64, DiskIdentity, Result<Telemetry, QueryErrorState>)>::new();
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let discovered = tokio::task::spawn_blocking(backend::discover)
                        .await
                        .unwrap_or_else(|_| Err("falha de execução".to_owned()));
                    match discovered {
                        Ok(disks) => {
                            scheduler.update_disks(disks);
                            let signal = AppSignal::Discovered {
                                generation: scheduler.generation,
                                result: Ok(scheduler.disks.clone()),
                            };
                            if sender.send(TerminalWindowMainThreadSignal::ApplyAppSignal(signal)).await.is_err() { break; }
                            while let Some((generation, identity)) = scheduler.begin_next() {
                                jobs.spawn(async move {
                                    let query_identity = identity.clone();
                                    let result = tokio::task::spawn_blocking(move || backend::query(&query_identity).map_err(|error| error.state))
                                        .await
                                        .unwrap_or(Err(QueryErrorState::Transient));
                                    // Keep the immutable query key for generation validation.
                                    (generation, identity, result)
                                });
                            }
                        }
                        Err(_) => {
                            let signal = AppSignal::Discovered {
                                generation: scheduler.generation,
                                result: Err(String::new()),
                            };
                            if sender.send(TerminalWindowMainThreadSignal::ApplyAppSignal(signal)).await.is_err() { break; }
                        }
                    }
                }
                Some(joined) = jobs.join_next(), if !jobs.is_empty() => {
                    if let Ok((generation, identity, result)) = joined {
                        if scheduler.finish(generation, &identity) {
                            let signal = AppSignal::QueryFinished {
                                generation,
                                disk_id: identity.id.clone(),
                                result,
                            };
                            if sender.send(TerminalWindowMainThreadSignal::ApplyAppSignal(signal)).await.is_err() { break; }
                        }
                        while let Some((generation, identity)) = scheduler.begin_next() {
                            jobs.spawn(async move {
                                let query_identity = identity.clone();
                                let result = tokio::task::spawn_blocking(move || backend::query(&query_identity).map_err(|error| error.state))
                                    .await
                                    .unwrap_or(Err(QueryErrorState::Transient));
                                (generation, identity, result)
                            });
                        }
                    }
                }
            }
        }
    });
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HitArea {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}
impl HitArea {
    fn contains(self, x: usize, y: usize) -> bool {
        self.width > 0
            && self.height > 0
            && x >= self.x
            && x < self.x + self.width
            && y >= self.y
            && y < self.y + self.height
    }
    fn visible(self) -> bool {
        self.width > 0 && self.height > 0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    pub tabs: Vec<HitArea>,
    pub tab_indices: Vec<usize>,
    pub quit: HitArea,
    pub body_y: usize,
}
impl Layout {
    pub fn new(width: usize, height: usize, disks: usize, scroll: usize) -> Self {
        if width == 0 || height == 0 {
            return Self::default();
        }
        let quit = if width >= 21 {
            HitArea {
                x: width - 7,
                y: 0,
                width: 7,
                height: 1,
            }
        } else if width >= 18 {
            HitArea {
                x: width - 4,
                y: 0,
                width: 4,
                height: 1,
            }
        } else {
            HitArea {
                x: width - 1,
                y: 0,
                width: 1,
                height: 1,
            }
        };
        let mut tabs = Vec::new();
        let mut tab_indices = Vec::new();
        let mut x = 0;
        let y = usize::from(height > 1);
        let start = scroll.min(disks.saturating_sub(1));
        if width >= 3 && height > 2 {
            for index in start..disks {
                let remaining = width.saturating_sub(x);
                if remaining < 22 && !tabs.is_empty() {
                    break;
                }
                let available = 22.min(remaining);
                if available == 0 {
                    break;
                }
                tabs.push(HitArea {
                    x,
                    y,
                    width: available,
                    height: 1,
                });
                tab_indices.push(index);
                x += available;
                if x >= width {
                    break;
                }
            }
        }
        let body_y = if height > 2 { 2 } else { height };
        Self {
            tabs,
            tab_indices,
            quit,
            body_y,
        }
    }
    pub fn tab_at(&self, x: usize, y: usize) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.contains(x, y))
            .and_then(|position| self.tab_indices.get(position).copied())
    }
}

fn fill_row(ops: &mut RenderOpIRVec, y: usize, width: usize, color: RgbValue) {
    if width > 0 {
        paint_background(
            ops,
            0,
            y,
            &" ".repeat(width),
            MOCHA_TEXT,
            false,
            width,
            Some(color),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn render_disk(
    ops: &mut RenderOpIRVec,
    disk: &DiskView,
    history: &History,
    top: usize,
    width: usize,
    available_height: usize,
    scroll: usize,
    frame: usize,
) {
    if available_height == 0 || width == 0 {
        return;
    }
    let mut line = 0usize;
    let mut row = |text: &str, fg: RgbValue, bg: RgbValue, bold: bool| {
        if line >= scroll && line - scroll < available_height {
            let y = top + line - scroll;
            fill_row(ops, y, width, bg);
            paint_background(
                ops,
                1.min(width.saturating_sub(1)),
                y,
                text,
                fg,
                bold,
                width.saturating_sub(1),
                Some(bg),
            );
        }
        line += 1;
    };
    let model = disk
        .identity
        .model
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Modelo desconhecido");
    row(model, MOCHA_TEXT, MOCHA_BASE, true);

    let Some(telemetry) = disk.telemetry.as_ref() else {
        let message = if disk.querying || disk.computing {
            if disk.computing {
                "Calculando saúde…"
            } else {
                "Consultando dados de saúde…"
            }
        } else {
            disk.error_message
                .as_deref()
                .unwrap_or("Dados de saúde indisponíveis")
        };
        let color = if disk.querying || disk.computing {
            MOCHA_YELLOW
        } else {
            MOCHA_RED
        };
        row(message, color, MOCHA_SURFACE0, true);
        let kind = match disk.identity.kind {
            DiskKind::Hdd => "HDD",
            DiskKind::Ssd => "SSD",
            DiskKind::Nvme => "NVMe",
            DiskKind::Unknown => "Tipo desconhecido",
        };
        row(
            &format!(
                "{kind} · {}",
                disk.identity
                    .capacity_bytes
                    .map(format_bytes)
                    .unwrap_or_else(|| "capacidade indisponível".into())
            ),
            MOCHA_MUTED,
            MOCHA_BASE,
            false,
        );
        return;
    };
    let health = score_health(disk.identity.kind, telemetry);
    let (label, color) = health_presentation(&health);
    let nominal_attention = health.outcome == HealthOutcome::Pass && health.score <= 50;
    let basic_smart = is_basic_smart_pass(&health);
    let large_score = available_height >= 9 && width >= 26;
    let headline = if large_score || health.outcome == HealthOutcome::Unknown {
        label.to_owned()
    } else if nominal_attention {
        format!("NOMINAL · {}%", health.score)
    } else if basic_smart {
        format!("SMART OK · {}%", health.score)
    } else {
        format!("{label} · {}%", health.score)
    };
    row(&headline, color, MOCHA_SURFACE0, true);

    if large_score {
        let score_text = if health.outcome == HealthOutcome::Unknown {
            "N/A".to_owned()
        } else {
            format!("{}%", health.score)
        };
        for score_row in ascii_score_lines(&score_text) {
            row(&score_row, color, MOCHA_BASE, true);
        }
    }
    let eta = if disk.identity.kind == DiskKind::Hdd || disk.identity.kind == DiskKind::Unknown {
        if width < 40 {
            "Vida estimada  n/a".to_owned()
        } else {
            "Vida estimada  não aplicável a este dispositivo".to_owned()
        }
    } else if width < 40 && history.estimate_endurance(&disk.identity.id).is_none() {
        "Vida estimada  sem histórico".to_owned()
    } else {
        history
            .estimate_endurance(&disk.identity.id)
            .map(|estimate| {
                format!(
                    "Vida estimada  {}",
                    format_endurance_days(estimate.remaining_seconds)
                )
            })
            .unwrap_or_else(|| "Vida estimada  aguardando histórico".into())
    };
    row(&eta, MOCHA_PEACH, MOCHA_MANTLE, true);

    let full_status = telemetry_status(telemetry);
    let status = if width >= 40 {
        full_status
    } else if full_status.contains("permissão") {
        "Permissão insuficiente"
    } else if health.outcome == HealthOutcome::Unknown {
        "Dados de saúde indisponíveis"
    } else if health.outcome == HealthOutcome::Fail {
        "Falha reportada"
    } else if full_status.contains("parcial") {
        "Dados parciais"
    } else {
        "Sem falha reportada"
    };
    let status_color =
        if full_status.contains("permissão") || matches!(health.outcome, HealthOutcome::Fail) {
            MOCHA_RED
        } else if health.outcome == HealthOutcome::Unknown
            || full_status.contains("parcial")
            || basic_smart
        {
            MOCHA_YELLOW
        } else {
            MOCHA_GREEN
        };
    row(status, status_color, MOCHA_SURFACE0, false);
    let kind = match disk.identity.kind {
        DiskKind::Hdd => "HDD",
        DiskKind::Ssd => "SSD",
        DiskKind::Nvme => "NVMe",
        DiskKind::Unknown => "Tipo desconhecido",
    };
    let bus = disk
        .identity
        .bus
        .as_deref()
        .unwrap_or("interface indisponível");
    row(
        &format!(
            "{kind} · {bus} · {}",
            disk.identity
                .capacity_bytes
                .map(format_bytes)
                .unwrap_or_else(|| "capacidade indisponível".into())
        ),
        MOCHA_MUTED,
        MOCHA_BASE,
        false,
    );
    let wear = telemetry
        .endurance_percent
        .map(|v| format!("{v:.0}%"))
        .unwrap_or_else(|| "—".into());
    let temperature = telemetry
        .temperature_c
        .map(|v| format!("{v} °C"))
        .unwrap_or_else(|| "—".into());
    let hours = telemetry
        .power_on_hours
        .map(|v| format!("{} h", compact_number(v)))
        .unwrap_or_else(|| "—".into());
    let metadata = if width < 40 {
        format!("Desg. {wear} · Temp. {temperature}")
    } else {
        format!("Desgaste {wear}   Temperatura {temperature}   Ligado {hours}")
    };
    row(&metadata, MOCHA_TEXT, MOCHA_BASE, false);
    if let QueryState::Success(nvme) = &telemetry.nvme {
        for (bit, warning) in [
            (1, "Reserva NVMe crítica"),
            (2, "Temperatura NVMe crítica"),
            (4, "Confiabilidade NVMe degradada"),
            (8, "Mídia NVMe somente leitura"),
            (16, "Aviso de memória NVMe"),
        ] {
            if nvme.critical_warning & bit != 0 {
                row(warning, MOCHA_RED, MOCHA_SURFACE0, true);
            }
        }
    }
    let _ = frame;
}

fn is_basic_smart_pass(health: &crate::health::HealthScore) -> bool {
    health.outcome == HealthOutcome::Pass
        && health.score == 100
        && health.confidence < 0.5
        && health
            .provenance
            .iter()
            .any(|detail| detail.contains("coarse"))
}

fn health_presentation(health: &crate::health::HealthScore) -> (&'static str, RgbValue) {
    match health.outcome {
        HealthOutcome::Unknown => ("SAÚDE INDISPONÍVEL", MOCHA_YELLOW),
        HealthOutcome::Fail => ("FALHA DETECTADA", MOCHA_RED),
        HealthOutcome::Pass if health.score <= 50 => ("ATENÇÃO · VIDA NOMINAL", MOCHA_YELLOW),
        HealthOutcome::Pass if is_basic_smart_pass(health) => {
            ("SMART OK · LEITURA BÁSICA", MOCHA_YELLOW)
        }
        HealthOutcome::Pass => ("SAUDÁVEL", MOCHA_GREEN),
    }
}

fn ascii_score_lines(value: &str) -> Vec<String> {
    let glyph = |ch: char| -> [&'static str; 5] {
        match ch {
            '0' => ["███", "█ █", "█ █", "█ █", "███"],
            '1' => [" █ ", "██ ", " █ ", " █ ", "███"],
            '2' => ["███", "  █", "███", "█  ", "███"],
            '3' => ["███", "  █", " ██", "  █", "███"],
            '4' => ["█ █", "█ █", "███", "  █", "  █"],
            '5' => ["███", "█  ", "███", "  █", "███"],
            '6' => ["███", "█  ", "███", "█ █", "███"],
            '7' => ["███", "  █", "  █", " █ ", " █ "],
            '8' => ["███", "█ █", "███", "█ █", "███"],
            '9' => ["███", "█ █", "███", "  █", "███"],
            'N' => ["█ █", "███", "███", "███", "█ █"],
            'A' => ["███", "█ █", "███", "█ █", "█ █"],
            '/' => ["  █", "  █", " █ ", "█  ", "█  "],
            '%' => ["█ █", "  █", " █ ", "█  ", "█ █"],
            _ => ["   "; 5],
        }
    };
    (0..5)
        .map(|row| {
            value
                .chars()
                .map(|ch| glyph(ch)[row])
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

fn telemetry_has_usable_data(telemetry: &Telemetry) -> bool {
    matches!(&telemetry.nvme, QueryState::Success(_))
        || matches!(&telemetry.ata, QueryState::Success(ata) if !ata.attributes.is_empty())
        || matches!(&telemetry.overall_failure, QueryState::Success(_))
        || telemetry.endurance_percent.is_some()
        || telemetry.power_on_hours.is_some()
        || telemetry.temperature_c.is_some()
}

fn telemetry_status(telemetry: &Telemetry) -> &'static str {
    if !telemetry_has_usable_data(telemetry) {
        if failed_permission(&telemetry.nvme)
            || failed_permission(&telemetry.ata)
            || failed_permission(&telemetry.overall_failure)
        {
            return "Permissão insuficiente para consultar o dispositivo";
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
    if failed_permission(&telemetry.nvme)
        || failed_permission(&telemetry.ata)
        || failed_permission(&telemetry.overall_failure)
    {
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
    "Dados SMART disponíveis"
}

fn failed_permission<T>(state: &QueryState<T>) -> bool {
    match state {
        QueryState::Failed { message } => is_permission_error(message),
        QueryState::NotQueried | QueryState::Unsupported | QueryState::Success(_) => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_background(
    ops: &mut RenderOpIRVec,
    x: usize,
    y: usize,
    text: &str,
    color: RgbValue,
    bold: bool,
    max_width: usize,
    background: Option<RgbValue>,
) {
    if max_width == 0 {
        return;
    }
    if let Some(background) = background {
        *ops += RenderOpCommon::SetBgColor(TuiColor::Rgb(background));
    }
    let background = background.unwrap_or(MOCHA_BASE);
    *ops += RenderOpCommon::SetFgColor(TuiColor::Rgb(color));
    let text = clipped(text, max_width);
    let style = TuiStyle {
        color_fg: Some(TuiColor::Rgb(color)),
        color_bg: Some(TuiColor::Rgb(background)),
        attribs: r3bl_tui::TuiStyleAttribs {
            bold: bold.then_some(r3bl_tui::tui_style_attrib::Bold),
            ..Default::default()
        },
        ..Default::default()
    };
    *ops += RenderOpCommon::MoveCursorPositionAbs(r3bl_tui::col(x) + r3bl_tui::row(y));
    *ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
    *ops += RenderOpCommon::SetBgColor(TuiColor::Rgb(MOCHA_BASE));
}

fn is_permission_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("access is denied")
        || message.contains("acesso negado")
        || message.contains("error 5")
        || message == "5"
}

fn clipped(text: &str, limit: usize) -> String {
    let graphemes = r3bl_tui::GCStringOwned::new(text);
    let mut display_width = 0;
    let mut end_byte = 0;
    for segment in graphemes.iter() {
        let segment_width = segment.display_width.as_usize();
        if display_width + segment_width > limit {
            break;
        }
        display_width += segment_width;
        end_byte = segment.end_byte_index.as_usize();
    }
    text[..end_byte].to_owned()
}

fn pad_to_columns(text: &str, width: usize) -> String {
    let text = clipped(text, width);
    let display_width = r3bl_tui::GCStringOwned::new(&text)
        .display_width()
        .as_usize();
    format!("{text}{}", " ".repeat(width.saturating_sub(display_width)))
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
#[cfg(test)]
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
        format!("{} dias", days)
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

fn history_path() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("chkdsk")
        .join("historico.json")
}
fn start_history_writer() -> UnboundedSender<History> {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(history_writer_loop(receiver, history_path()));
    sender
}

async fn history_writer_loop(mut receiver: UnboundedReceiver<History>, path: PathBuf) {
    while let Some(mut newest) = receiver.recv().await {
        // Snapshots are sent in UI-event order. Drain queued snapshots before each
        // write so a slow filesystem persists the newest state, never an older one.
        while let Ok(next) = receiver.try_recv() {
            newest = next;
        }
        let destination = path.clone();
        let _ = tokio::task::spawn_blocking(move || write_history_snapshot(&destination, &newest))
            .await;
    }
}

fn write_history_snapshot(path: &std::path::Path, history: &History) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let Some(file_name) = path.file_name() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "caminho do histórico sem nome de arquivo",
        ));
    };
    let mut temporary_name = file_name.to_os_string();
    temporary_name.push(format!(".tmp-{}", std::process::id()));
    let temporary = path.with_file_name(temporary_name);
    let write_result = (|| {
        let bytes = serde_json::to_vec(history).map_err(std::io::Error::other)?;
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write_result
}

#[cfg(windows)]
fn replace_file(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    let replaced = unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING
                | windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}
fn next_timestamp(history: &History, disk_id: &str) -> Option<u64> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    next_timestamp_at(history, disk_id, now)
}

fn next_timestamp_at(history: &History, disk_id: &str, now: u64) -> Option<u64> {
    let latest = history
        .samples
        .iter()
        .filter(|sample| sample.disk_id == disk_id)
        .map(|sample| sample.timestamp_unix_secs)
        .max();
    match latest {
        Some(latest) if now <= latest => None,
        _ => Some(now),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_global_data(
        state: Dashboard,
        width: usize,
        height: usize,
    ) -> GlobalData<Dashboard, AppSignal> {
        let (sender, _receiver) = tokio::sync::mpsc::channel(1);
        let window_size = r3bl_tui::Size::new((
            r3bl_tui::ColWidth::new(width),
            r3bl_tui::RowHeight::new(height),
        ));
        GlobalData::try_to_create_instance(
            sender,
            state,
            window_size,
            r3bl_tui::OutputDevice::new_stderr(),
            r3bl_tui::OffscreenBufferPool::new(window_size),
        )
        .unwrap()
    }

    fn dispatch_input(
        app: &mut DashboardApp,
        global: &mut GlobalData<Dashboard, AppSignal>,
        event: InputEvent,
    ) -> EventPropagation {
        app.app_handle_input_event(
            event,
            global,
            &mut ComponentRegistryMap::default(),
            &mut HasFocus::default(),
        )
        .unwrap()
    }

    fn mouse_event(kind: MouseInputKind, x: usize, y: usize) -> InputEvent {
        InputEvent::Mouse(r3bl_tui::MouseInput {
            pos: r3bl_tui::col(x) + r3bl_tui::row(y),
            kind,
            maybe_modifier_keys: None,
        })
    }

    fn rgb_hex(color: TuiColor) -> String {
        match color {
            TuiColor::Rgb(value) => {
                format!("#{:02x}{:02x}{:02x}", value.red, value.green, value.blue)
            }
            other => panic!("unexpected non-truecolor UI cell: {other:?}"),
        }
    }

    fn is_mocha_color(color: TuiColor) -> bool {
        matches!(color, TuiColor::Rgb(value) if [
            MOCHA_BASE, MOCHA_MANTLE, MOCHA_SURFACE0, MOCHA_SURFACE1, MOCHA_SURFACE2,
            MOCHA_TEXT, MOCHA_MUTED, MOCHA_LAVENDER, MOCHA_GREEN, MOCHA_RED,
            MOCHA_YELLOW, MOCHA_PEACH,
        ].contains(&value))
    }

    fn cell_has_colors(
        pixel: &r3bl_tui::PixelChar,
        foreground: RgbValue,
        background: RgbValue,
    ) -> bool {
        matches!(pixel, r3bl_tui::PixelChar::PlainText { style, .. }
            if style.color_fg == Some(TuiColor::Rgb(foreground))
                && style.color_bg == Some(TuiColor::Rgb(background)))
    }

    fn escape_cell(ch: char) -> &'static str {
        match ch {
            '&' => "&amp;",
            '<' => "&lt;",
            '>' => "&gt;",
            '\'' => "&#39;",
            '"' => "&quot;",
            _ => "",
        }
    }

    #[test]
    fn rendered_cells_cover_viewport_with_explicit_catppuccin_truecolor_and_optional_browser_proof()
    {
        let mut nvme = Telemetry::default();
        nvme.apply_nvme(crate::model::NvmeHealth {
            percentage_used: Some(12),
            ..Default::default()
        });
        let fixtures = [
            ("NVMe saudável", DiskKind::Nvme, "M.2 NVMe", "NVMe", nvme),
            (
                "SATA SSD",
                DiskKind::Ssd,
                "SSD SATA",
                "SATA",
                Telemetry {
                    endurance_percent: Some(12.0),
                    ata: QueryState::Success(crate::model::AtaHealth::default()),
                    ..Default::default()
                },
            ),
            (
                "SATA HDD",
                DiskKind::Hdd,
                "Disco rígido",
                "SATA",
                Telemetry {
                    ata: QueryState::Success(crate::model::AtaHealth::default()),
                    overall_failure: QueryState::Success(false),
                    ..Default::default()
                },
            ),
            (
                "Saúde desconhecida",
                DiskKind::Unknown,
                "Dispositivo",
                "USB",
                Telemetry::default(),
            ),
            (
                "Dados parciais",
                DiskKind::Ssd,
                "SSD parcial",
                "SATA",
                Telemetry {
                    nvme: QueryState::Failed {
                        message: "Access is denied (5)".into(),
                    },
                    ata: QueryState::Success(crate::model::AtaHealth::default()),
                    ..Default::default()
                },
            ),
        ];
        let mut html = String::from(
            "<!doctype html><meta charset=\"utf-8\"><title>chkdsk UI cell proof</title><style>body{margin:24px;background:#11111b;color:#cdd6f4;font:14px system-ui,sans-serif}h1{font-size:18px;font-weight:600;margin:0 0 20px}section{margin:0 0 30px}.case{color:#a6adc8;margin:8px 0 5px}.viewport{overflow:auto;display:inline-block;background:#1e1e2e;padding:2px}.row{display:grid;grid-auto-rows:16px}.cell{display:block;width:8px;height:16px;padding:0;margin:0;overflow:hidden;font-family:\"Cascadia Mono\",\"Cascadia Code\",Consolas,monospace;font-size:13px;line-height:16px;letter-spacing:0;font-kerning:none;white-space:pre}.cell.wide{grid-column:span 2;width:16px}</style><h1>chkdsk · celdas do buffer offscreen</h1>",
        );
        let output_dir = std::env::var_os("CHKDSK_UI_PROOF_DIR").map(PathBuf::from);
        for (fixture_name, kind, model, bus, telemetry) in fixtures {
            for (width, height) in [(100, 28), (70, 20), (30, 10)] {
                let disk = DiskView {
                    identity: DiskIdentity {
                        id: fixture_name.into(),
                        index: 4,
                        model: Some(model.into()),
                        serial: None,
                        firmware: None,
                        capacity_bytes: Some(1_000_000_000_000),
                        bus: Some(bus.into()),
                        kind,
                    },
                    telemetry: Some(telemetry.clone()),
                    querying: false,
                    computing: false,
                    pending_telemetry: None,
                    error_message: None,
                };
                let mut sata_id = identity("tab-sata-ssd", 8);
                sata_id.model = Some("SSD SATA Samsung".into());
                sata_id.bus = Some("SATA".into());
                sata_id.kind = DiskKind::Ssd;
                let mut unicode_id = identity("tab-wide-nvme", 12);
                unicode_id.model = Some("🧪漢字e\u{301} NVMe".into());
                unicode_id.bus = Some("NVMe".into());
                unicode_id.kind = DiskKind::Nvme;
                let state = Dashboard {
                    disks: vec![
                        disk,
                        DiskView {
                            identity: sata_id,
                            telemetry: None,
                            querying: false,
                            computing: false,
                            pending_telemetry: None,
                            error_message: None,
                        },
                        DiskView {
                            identity: unicode_id,
                            telemetry: None,
                            querying: false,
                            computing: false,
                            pending_telemetry: None,
                            error_message: None,
                        },
                    ],
                    ..Default::default()
                };
                let mut app = DashboardApp {
                    started: true,
                    ..Default::default()
                };
                let mut global = test_global_data(state, width, height);
                let pipeline = app
                    .app_render(
                        &mut global,
                        &mut ComponentRegistryMap::default(),
                        &mut HasFocus::default(),
                    )
                    .unwrap();
                let size = r3bl_tui::Size::new((
                    r3bl_tui::ColWidth::new(width),
                    r3bl_tui::RowHeight::new(height),
                ));
                let mut buffer = r3bl_tui::OffscreenBuffer::new_empty(size);
                pipeline.compose_render_ops_into_ofs_buf(
                    size,
                    &mut buffer,
                    &mut r3bl_tui::MemoizedLenMap::default(),
                );
                assert_eq!(buffer.len(), height);
                assert!(buffer.iter().all(|row| row.len() == width));
                html.push_str(&format!("<section><div class=\"case\">{fixture_name} · {width} × {height}</div><div class=\"viewport\">"));
                for cells in buffer.iter() {
                    html.push_str(&format!(
                        "<div class=\"row\" style=\"grid-template-columns:repeat({width},8px)\">"
                    ));
                    let mut cell_iter = cells.iter().peekable();
                    while let Some(cell) = cell_iter.next() {
                        let r3bl_tui::PixelChar::PlainText {
                            display_char,
                            style,
                        } = cell
                        else {
                            panic!(
                                "unpainted cell in {fixture_name} at {width}x{height}: {cell:?}"
                            );
                        };
                        let fg = style
                            .color_fg
                            .map(rgb_hex)
                            .expect("cell without explicit foreground");
                        let bg = style
                            .color_bg
                            .map(rgb_hex)
                            .expect("cell without explicit background");
                        assert!(fg.starts_with('#') && bg.starts_with('#'));
                        assert!(is_mocha_color(style.color_fg.unwrap()));
                        assert!(is_mocha_color(style.color_bg.unwrap()));
                        let glyph_width = r3bl_tui::GCStringOwned::width_char(*display_char)
                            .as_usize()
                            .max(1);
                        if glyph_width == 2
                            && matches!(cell_iter.peek(), Some(r3bl_tui::PixelChar::Void))
                        {
                            cell_iter.next();
                        }
                        let escaped = escape_cell(*display_char);
                        let ch = if escaped.is_empty() {
                            display_char.to_string()
                        } else {
                            escaped.into()
                        };
                        let cell_class = if glyph_width == 2 {
                            "cell wide"
                        } else {
                            "cell"
                        };
                        html.push_str(&format!("<span class=\"{cell_class}\" style=\"color:{fg};background:{bg}\">{ch}</span>"));
                    }
                    html.push_str("</div>");
                }
                html.push_str("</div></section>");
            }
        }
        if let Some(output_dir) = output_dir {
            std::fs::create_dir_all(&output_dir).unwrap();
            let path = output_dir.join("chkdsk-ui-proof.html");
            std::fs::write(&path, html).unwrap();
            eprintln!("CHkdsk UI proof: {}", path.display());
        }
    }

    #[test]
    fn horizontal_tabs_keep_earlier_disks_visible_until_selection_overflows_and_hover_is_distinct()
    {
        let mut state = Dashboard {
            disks: (0..9)
                .map(|index| {
                    let mut disk_identity = identity(&format!("disk-{index}"), index);
                    if index == 1 {
                        disk_identity.model = Some("🧪漢字e\u{301} NVMe".into());
                    }
                    DiskView {
                        identity: disk_identity,
                        telemetry: None,
                        querying: false,
                        computing: false,
                        pending_telemetry: None,
                        error_message: None,
                    }
                })
                .collect(),
            ..Default::default()
        };
        let visible = Layout::new(100, 28, state.disks.len(), state.scroll);
        assert_eq!(visible.tab_indices, vec![0, 1, 2, 3]);
        assert_eq!(visible.tab_at(24, 1), Some(1));
        assert_eq!(visible.tab_at(43, 1), Some(1));
        assert_eq!(visible.tab_at(44, 1), Some(2));
        state.selected = 4;
        ensure_selected_visible(&mut state, 100, 28);
        let shifted = Layout::new(100, 28, state.disks.len(), state.scroll);
        assert_eq!(shifted.tab_indices, vec![1, 2, 3, 4]);
        assert_eq!(shifted.tab_at(0, 1), Some(1));
        assert_eq!(shifted.tab_at(3 * 22, 1), Some(4));

        let mut app = DashboardApp {
            started: true,
            ..Default::default()
        };
        let mut global = test_global_data(
            Dashboard {
                disks: state.disks,
                ..Default::default()
            },
            100,
            28,
        );
        assert!(matches!(
            dispatch_input(
                &mut app,
                &mut global,
                mouse_event(MouseInputKind::MouseMove, 24, 1)
            ),
            EventPropagation::ConsumedRender
        ));
        assert_eq!(global.state.hovered_tab, Some(1));
        let pipeline = app
            .app_render(
                &mut global,
                &mut ComponentRegistryMap::default(),
                &mut HasFocus::default(),
            )
            .unwrap();
        let size =
            r3bl_tui::Size::new((r3bl_tui::ColWidth::new(100), r3bl_tui::RowHeight::new(28)));
        let mut buffer = r3bl_tui::OffscreenBuffer::new_empty(size);
        pipeline.compose_render_ops_into_ofs_buf(
            size,
            &mut buffer,
            &mut r3bl_tui::MemoizedLenMap::default(),
        );
        assert!(
            matches!(buffer[1][24], r3bl_tui::PixelChar::PlainText { style, .. } if style.color_bg == Some(TuiColor::Rgb(MOCHA_SURFACE2)))
        );
    }

    fn identity(id: &str, index: u32) -> DiskIdentity {
        DiskIdentity {
            id: id.into(),
            index,
            model: None,
            serial: None,
            firmware: None,
            capacity_bytes: None,
            bus: None,
            kind: DiskKind::Unknown,
        }
    }

    #[test]
    fn terminal_labels_clip_graphemes_by_display_columns_and_keep_tab_index_visible() {
        let model = "🧪漢字e\u{301} NVMe";
        assert_eq!(clipped(model, 4), "🧪漢");
        assert_eq!(clipped("e\u{301}SSD", 1), "e\u{301}");
        let short_clip = clipped(model, 4);
        let model_clip = clipped(model, 14);
        let model_width = r3bl_tui::GCStringOwned::new(&model_clip)
            .display_width()
            .as_usize();
        assert_eq!(
            pad_to_columns(&format!("> {model_clip} #12"), 22),
            format!(
                "> {model_clip} #12{}",
                " ".repeat(22 - 2 - model_width - 1 - 3)
            )
        );
        assert_eq!(
            r3bl_tui::GCStringOwned::new(short_clip)
                .display_width()
                .as_usize(),
            4
        );
    }

    #[test]
    fn health_visual_labels_distinguish_nominal_wear_from_device_failure_and_coarse_smart() {
        let nominal = crate::health::HealthScore {
            score: 0,
            outcome: HealthOutcome::Pass,
            confidence: 0.8,
            provenance: vec!["NVMe endurance used 100%".into()],
        };
        assert_eq!(
            health_presentation(&nominal),
            ("ATENÇÃO · VIDA NOMINAL", MOCHA_YELLOW)
        );
        let coarse = crate::health::HealthScore {
            score: 100,
            outcome: HealthOutcome::Pass,
            confidence: 0.1,
            provenance: vec!["device-reported no threshold failure (coarse only)".into()],
        };
        assert_eq!(
            health_presentation(&coarse),
            ("SMART OK · LEITURA BÁSICA", MOCHA_YELLOW)
        );
        let healthy = crate::health::HealthScore {
            score: 88,
            outcome: HealthOutcome::Pass,
            confidence: 0.8,
            provenance: vec!["NVMe SMART/Health log".into()],
        };
        assert_eq!(health_presentation(&healthy), ("SAUDÁVEL", MOCHA_GREEN));
        let failed = crate::health::HealthScore {
            score: 20,
            outcome: HealthOutcome::Fail,
            confidence: 1.0,
            provenance: vec!["device-reported overall failure".into()],
        };
        assert_eq!(health_presentation(&failed), ("FALHA DETECTADA", MOCHA_RED));
    }

    #[test]
    fn layouts_and_tab_hits_stay_inside_viewport_without_overlap() {
        for (width, height) in [(0, 0), (1, 1), (4, 2), (12, 6), (80, 24)] {
            let layout = Layout::new(width, height, 100, 20);
            let areas = layout
                .tabs
                .iter()
                .copied()
                .chain(std::iter::once(layout.quit))
                .filter(|area| area.visible())
                .collect::<Vec<_>>();
            for (index, area) in areas.iter().enumerate() {
                assert!(area.x + area.width <= width && area.y + area.height <= height);
                for other in areas.iter().skip(index + 1) {
                    assert!(
                        area.x + area.width <= other.x
                            || other.x + other.width <= area.x
                            || area.y + area.height <= other.y
                            || other.y + other.height <= area.y
                    );
                }
            }
            for (position, tab) in layout.tabs.iter().enumerate() {
                assert_eq!(
                    layout.tab_at(tab.x, tab.y),
                    Some(layout.tab_indices[position])
                );
            }
            if layout.quit.visible() {
                assert!(layout.quit.contains(layout.quit.x, layout.quit.y));
                assert!(!layout.quit.contains(width, layout.quit.y));
            }
        }
        for index in 0..100 {
            let one_disk_tab = Layout::new(12, 6, 100, index);
            assert_eq!(one_disk_tab.tab_at(0, 1), Some(index));
        }
    }

    #[test]
    fn ui_generated_messages_are_brazilian_portuguese() {
        assert_eq!(format_duration(86_400), "1 dia");
        assert_eq!(format_duration(366 * 86_400), "1 ano");
        assert_eq!(format_endurance_days(0), "0 dias");
        assert_eq!(format_endurance_days(86_400), "1 dia");
        assert_eq!(format_endurance_days(3 * 86_400), "3 dias");
        assert_eq!(compact_number(1_234_567), "1,2 mi");
        let disk = DiskView {
            identity: DiskIdentity {
                id: "id".into(),
                index: 0,
                model: None,
                serial: None,
                firmware: None,
                capacity_bytes: None,
                bus: None,
                kind: DiskKind::Unknown,
            },
            telemetry: None,
            querying: false,
            computing: false,
            pending_telemetry: None,
            error_message: None,
        };
        let mut ops = RenderOpIRVec::new();
        render_disk(&mut ops, &disk, &History::default(), 0, 64, 12, 0, 0);
        let shown = format!("{ops:?}");
        assert!(shown.contains("Modelo desconhecido"));
        assert!(shown.contains("capacidade indisponível"));
        assert!(shown.contains("Dados de saúde indisponíveis"));

        let unknown = DiskView {
            telemetry: Some(Telemetry::default()),
            ..disk
        };
        let mut unknown_ops = RenderOpIRVec::new();
        render_disk(
            &mut unknown_ops,
            &unknown,
            &History::default(),
            0,
            80,
            18,
            0,
            0,
        );
        let unknown_screen = format!("{unknown_ops:?}");
        assert!(unknown_screen.contains("INDISPONÍVEL"));
        assert!(unknown_screen.contains("N/A"));
        assert!(!unknown_screen.contains("% saúde"));
        assert!(!unknown_screen.contains("0 / 100"));
    }

    #[test]
    fn keyboard_wheel_click_and_compact_quit_dispatch_real_input_events() {
        let state = Dashboard {
            disks: (0..4)
                .map(|index| DiskView {
                    identity: identity(&index.to_string(), index),
                    telemetry: None,
                    querying: false,
                    computing: false,
                    pending_telemetry: None,
                    error_message: None,
                })
                .collect(),
            ..Default::default()
        };
        let mut app = DashboardApp::default();
        let mut global = test_global_data(state, 80, 24);

        assert!(matches!(
            dispatch_input(
                &mut app,
                &mut global,
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Tab),
                }),
            ),
            EventPropagation::ConsumedRender
        ));
        assert_eq!(global.state.selected, 1);
        assert_eq!(global.state.scroll, 0);
        for key in [SpecialKey::Right, SpecialKey::BackTab] {
            dispatch_input(
                &mut app,
                &mut global,
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(key),
                }),
            );
        }
        assert_eq!(global.state.selected, 1);

        assert!(matches!(
            dispatch_input(
                &mut app,
                &mut global,
                mouse_event(MouseInputKind::ScrollDown, 0, 8),
            ),
            EventPropagation::ConsumedRender
        ));
        assert_eq!(global.state.selected, 1);
        assert_eq!(global.state.content_scroll, 1);

        let layout = Layout::new(80, 24, global.state.disks.len(), global.state.scroll);
        let next_tab = layout.tabs[2];
        assert_eq!(layout.tab_at(next_tab.x, next_tab.y), Some(2));
        assert!(matches!(
            dispatch_input(
                &mut app,
                &mut global,
                mouse_event(
                    MouseInputKind::MouseDown(Button::Left),
                    next_tab.x,
                    next_tab.y
                ),
            ),
            EventPropagation::ConsumedRender
        ));
        assert_eq!(global.state.selected, 2);
        assert_eq!(global.state.scroll, 0);

        let compact_state = global.state.clone();
        let mut compact = test_global_data(compact_state, 12, 2);
        let compact_layout = Layout::new(12, 2, compact.state.disks.len(), compact.state.scroll);
        assert!(compact_layout.tabs.is_empty());
        assert!(matches!(
            dispatch_input(
                &mut app,
                &mut compact,
                mouse_event(
                    MouseInputKind::MouseDown(Button::Left),
                    compact_layout.quit.x,
                    compact_layout.quit.y,
                ),
            ),
            EventPropagation::ExitMainEventLoop
        ));
        assert_eq!(compact.state.selected, 2);
        assert_eq!(compact.state.scroll, 0);

        let mut tiny = test_global_data(compact.state.clone(), 1, 1);
        let tiny_layout = Layout::new(1, 1, tiny.state.disks.len(), tiny.state.scroll);
        assert_eq!(
            tiny_layout.quit,
            HitArea {
                x: 0,
                y: 0,
                width: 1,
                height: 1
            }
        );
        assert!(matches!(
            dispatch_input(
                &mut app,
                &mut tiny,
                mouse_event(MouseInputKind::MouseDown(Button::Left), 0, 0),
            ),
            EventPropagation::ExitMainEventLoop
        ));
    }

    #[test]
    fn partial_telemetry_is_not_mislabeled_as_unsupported() {
        let telemetry = Telemetry {
            nvme: QueryState::Unsupported,
            ata: QueryState::Success(crate::model::AtaHealth {
                attributes: vec![crate::model::AtaAttribute {
                    id: 9,
                    flags: 0,
                    current: 100,
                    worst: 100,
                    raw: [0; 6],
                    threshold: None,
                }],
                ..Default::default()
            }),
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
            ata: QueryState::Success(crate::model::AtaHealth::default()),
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
            ata: QueryState::Success(crate::model::AtaHealth {
                attributes: vec![crate::model::AtaAttribute {
                    id: 9,
                    flags: 0,
                    current: 100,
                    worst: 100,
                    raw: [0; 6],
                    threshold: None,
                }],
                ..Default::default()
            }),
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

        let no_data = Telemetry {
            nvme: QueryState::Failed {
                message: "Access is denied".into(),
            },
            ..Default::default()
        };
        assert_eq!(
            telemetry_status(&no_data),
            "Permissão insuficiente para consultar o dispositivo"
        );
    }

    #[test]
    fn wall_clock_rollback_holds_history_baseline_until_time_advances() {
        let mut history = History::default();
        history
            .record(HealthSample {
                disk_id: "ssd-a".into(),
                timestamp_unix_secs: 2_000,
                kind: DiskKind::Ssd,
                endurance_percent: Some(10.0),
                power_on_hours: None,
                score: Some(94),
            })
            .unwrap();

        assert_eq!(next_timestamp_at(&history, "ssd-a", 1_000), None);
        assert_eq!(next_timestamp_at(&history, "ssd-a", 2_000), None);
        assert_eq!(next_timestamp_at(&history, "ssd-a", 2_001), Some(2_001));
        assert_eq!(next_timestamp_at(&history, "other", 1_000), Some(1_000));
        assert_eq!(history.estimate_endurance("ssd-a"), None);
    }

    #[test]
    fn unknown_health_keeps_historical_eta_in_compact_and_normal_views() {
        let mut history = History::default();
        for (timestamp, wear) in [(1_000_000, 10.0), (1_086_400, 11.0)] {
            history
                .record(HealthSample {
                    disk_id: "test".into(),
                    timestamp_unix_secs: timestamp,
                    kind: DiskKind::Nvme,
                    endurance_percent: Some(wear),
                    power_on_hours: None,
                    score: None,
                })
                .unwrap();
        }

        for (width, height, querying) in [(12, 8, false), (12, 8, true), (64, 20, false)] {
            let state = Dashboard {
                disks: vec![DiskView {
                    identity: identity("test", 0),
                    telemetry: Some(Telemetry::default()),
                    querying,
                    computing: false,
                    pending_telemetry: None,
                    error_message: None,
                }],
                history: history.clone(),
                ..Default::default()
            };
            let mut app = DashboardApp {
                started: true,
                ..Default::default()
            };
            let mut global = test_global_data(state, width, height);
            let pipeline = app
                .app_render(
                    &mut global,
                    &mut ComponentRegistryMap::default(),
                    &mut HasFocus::default(),
                )
                .unwrap();
            let size = r3bl_tui::Size::new((
                r3bl_tui::ColWidth::new(width),
                r3bl_tui::RowHeight::new(height),
            ));
            let mut buffer = r3bl_tui::OffscreenBuffer::new_empty(size);
            pipeline.compose_render_ops_into_ofs_buf(
                size,
                &mut buffer,
                &mut r3bl_tui::MemoizedLenMap::default(),
            );
            let rendered = buffer
                .iter()
                .map(|line| {
                    line.iter()
                        .map(|pixel| match pixel {
                            r3bl_tui::PixelChar::PlainText { display_char, .. } => *display_char,
                            _ => ' ',
                        })
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                rendered.contains("Vida estima"),
                "{width}x{height}: {rendered}"
            );
        }
    }

    #[test]
    fn rendered_dashboard_has_brand_large_health_and_safe_compact_fallbacks() {
        let telemetry = Telemetry {
            nvme: QueryState::Success(crate::model::NvmeHealth {
                percentage_used: Some(12),
                ..Default::default()
            }),
            endurance_percent: Some(12.0),
            ..Default::default()
        };
        let disk = DiskView {
            identity: identity("test", 4),
            telemetry: Some(telemetry),
            querying: false,
            computing: false,
            pending_telemetry: None,
            error_message: None,
        };
        let mut second_identity = identity("sata", 8);
        second_identity.model = Some("SSD SATA".into());
        second_identity.kind = DiskKind::Ssd;
        let mut third_identity = identity("wide", 12);
        third_identity.model = Some("🧪漢字e\u{301} NVMe".into());
        third_identity.kind = DiskKind::Nvme;
        let state = Dashboard {
            disks: vec![
                disk,
                DiskView {
                    identity: second_identity,
                    telemetry: None,
                    querying: false,
                    computing: false,
                    pending_telemetry: None,
                    error_message: None,
                },
                DiskView {
                    identity: third_identity,
                    telemetry: None,
                    querying: false,
                    computing: false,
                    pending_telemetry: None,
                    error_message: None,
                },
            ],
            ..Default::default()
        };
        let mut app = DashboardApp {
            started: true,
            ..Default::default()
        };
        for (width, height) in [(100, 28), (12, 4), (1, 1), (1, 8)] {
            let mut global = test_global_data(state.clone(), width, height);
            let pipeline = app
                .app_render(
                    &mut global,
                    &mut ComponentRegistryMap::default(),
                    &mut HasFocus::default(),
                )
                .unwrap();
            let mut buffer = r3bl_tui::OffscreenBuffer::new_empty(r3bl_tui::Size::new((
                r3bl_tui::ColWidth::new(width),
                r3bl_tui::RowHeight::new(height),
            )));
            pipeline.compose_render_ops_into_ofs_buf(
                r3bl_tui::Size::new((
                    r3bl_tui::ColWidth::new(width),
                    r3bl_tui::RowHeight::new(height),
                )),
                &mut buffer,
                &mut r3bl_tui::MemoizedLenMap::default(),
            );
            if width == 100 {
                assert!(
                    matches!(
                        buffer[1][0],
                        r3bl_tui::PixelChar::PlainText { style, .. }
                            if style.color_bg == Some(TuiColor::Rgb(MOCHA_LAVENDER))
                    ),
                    "bg={:?} wanted={:?}",
                    match buffer[1][0] {
                        r3bl_tui::PixelChar::PlainText { style, .. } => style.color_bg,
                        _ => None,
                    },
                    TuiColor::Rgb(MOCHA_LAVENDER)
                );
            }
            let rendered = buffer
                .iter()
                .map(|line| {
                    line.iter()
                        .map(|pixel| match pixel {
                            r3bl_tui::PixelChar::PlainText { display_char, .. } => *display_char,
                            _ => ' ',
                        })
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            if width >= 21 {
                assert!(
                    rendered.contains("chkdsk v"),
                    "{width}x{height}: {rendered}"
                );
            }
            if width == 100 {
                assert!(cell_has_colors(&buffer[0][1], MOCHA_LAVENDER, MOCHA_MANTLE));
                assert!(cell_has_colors(&buffer[1][0], MOCHA_BASE, MOCHA_LAVENDER));
                assert!(cell_has_colors(&buffer[3][1], MOCHA_GREEN, MOCHA_SURFACE0));
                assert!(cell_has_colors(&buffer[4][1], MOCHA_GREEN, MOCHA_BASE));
                assert!(cell_has_colors(&buffer[9][1], MOCHA_PEACH, MOCHA_MANTLE));
                assert!(
                    rendered.contains("#4") && rendered.contains("#8") && rendered.contains("#12"),
                    "{rendered}"
                );
                assert!(rendered.contains("SAUDÁVEL"));
                assert!(
                    ascii_score_lines("88%")
                        .iter()
                        .all(|score_row| rendered.contains(score_row.trim_end()))
                );
            }
            if width == 12 && height >= 8 {
                assert!(rendered.contains("88%"), "{rendered}");
                assert!(rendered.contains("Vida estima"), "{rendered}");
            }
        }
        let unknown = Telemetry::default();
        assert_eq!(
            score_health(DiskKind::Unknown, &unknown).outcome,
            HealthOutcome::Unknown
        );
        let digits = ascii_score_lines("N/A").join("\n");
        assert!(!digits.contains("000"));
        let permission = Telemetry {
            nvme: QueryState::Failed {
                message: "Access is denied (5)".into(),
            },
            ..Default::default()
        };
        assert_eq!(
            telemetry_status(&permission),
            "Permissão insuficiente para consultar o dispositivo"
        );
    }

    #[test]
    fn slow_queries_are_bounded_deduplicated_and_stale_hotplug_results_requeued() {
        let disks: Vec<DiskIdentity> = (0..8).map(|i| identity(&format!("disk-{i}"), i)).collect();
        let mut scheduler = QueryScheduler::default();
        scheduler.update_disks(disks.clone());
        let topology_generation = scheduler.generation;
        let first = (0..MAX_CONCURRENT_DISK_QUERIES)
            .map(|_| scheduler.begin_next().unwrap())
            .collect::<Vec<_>>();
        assert!(scheduler.begin_next().is_none());
        assert_eq!(scheduler.in_flight.len(), MAX_CONCURRENT_DISK_QUERIES);

        // Repeated polling cannot enqueue an already-running disk a second time.
        scheduler.update_disks(disks.clone());
        assert_eq!(scheduler.generation, topology_generation);
        assert_eq!(scheduler.in_flight.len(), MAX_CONCURRENT_DISK_QUERIES);
        assert!(
            scheduler
                .pending
                .iter()
                .all(|disk| !scheduler.in_flight.contains_key(&disk.id))
        );
        assert!(scheduler.finish(first[1].0, &first[1].1));

        let stale_identity = first[0].1.clone();
        let mut changed = disks;
        changed[0] = identity(&stale_identity.id, 99);
        scheduler.update_disks(changed);
        assert_ne!(scheduler.generation, topology_generation);
        assert!(!scheduler.finish(first[0].0, &stale_identity));
        assert!(
            scheduler
                .pending
                .iter()
                .any(|disk| disk.id == stale_identity.id && disk.index == 99)
        );
        assert!(scheduler.begin_next().is_some());
        assert!(scheduler.in_flight.len() <= MAX_CONCURRENT_DISK_QUERIES);
    }

    #[tokio::test]
    async fn history_snapshots_are_serialized_atomic_and_end_at_the_newest_state() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "chkdsk-history-{}-{unique}.json",
            std::process::id()
        ));
        write_history_snapshot(
            &path,
            &History {
                version: 0,
                samples: Vec::new(),
            },
        )
        .unwrap();
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let writer = tokio::spawn(history_writer_loop(receiver, path.clone()));
        for version in 1..=5 {
            sender
                .send(History {
                    version,
                    samples: Vec::new(),
                })
                .unwrap();
        }
        drop(sender);
        writer.await.unwrap();

        let persisted: History = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted.version, 5);
        let temporary = path.with_file_name(format!(
            "{}.tmp-{}",
            path.file_name().unwrap().to_string_lossy(),
            std::process::id()
        ));
        assert!(!temporary.exists(), "arquivo temporário remanescente");
        std::fs::remove_file(path).unwrap();
    }
}
