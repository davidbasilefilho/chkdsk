//! Interface terminal em português brasileiro. A tradução fica nesta fronteira:
//! detalhes e mensagens do sistema operacional nunca são exibidos diretamente.

use crate::{
    backend::{self, QueryErrorState},
    health::{HealthOutcome, score_health},
    history::{HealthSample, History},
    model::{DiskIdentity, DiskKind, HealthWarningReason, QueryState, Telemetry},
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

const TOKYO_BG: RgbValue = RgbValue {
    red: 34,
    green: 36,
    blue: 54,
};
const TOKYO_BG_DARK: RgbValue = RgbValue {
    red: 30,
    green: 32,
    blue: 48,
};
const TOKYO_BG_DARK1: RgbValue = RgbValue {
    red: 25,
    green: 27,
    blue: 41,
};
const TOKYO_BG_HIGHLIGHT: RgbValue = RgbValue {
    red: 47,
    green: 51,
    blue: 77,
};
const TOKYO_BG_VISUAL: RgbValue = RgbValue {
    red: 45,
    green: 63,
    blue: 118,
};
const TOKYO_FG: RgbValue = RgbValue {
    red: 200,
    green: 211,
    blue: 245,
};
const TOKYO_FG_DARK: RgbValue = RgbValue {
    red: 130,
    green: 139,
    blue: 184,
};
const TOKYO_BLUE: RgbValue = RgbValue {
    red: 130,
    green: 170,
    blue: 255,
};
const TOKYO_GREEN: RgbValue = RgbValue {
    red: 195,
    green: 232,
    blue: 141,
};
const TOKYO_RED: RgbValue = RgbValue {
    red: 255,
    green: 117,
    blue: 127,
};
const TOKYO_YELLOW: RgbValue = RgbValue {
    red: 255,
    green: 199,
    blue: 119,
};
const TOKYO_ORANGE: RgbValue = RgbValue {
    red: 255,
    green: 150,
    blue: 108,
};
const TOKYO_PURPLE: RgbValue = RgbValue {
    red: 252,
    green: 167,
    blue: 234,
};
const TOKYO_CYAN: RgbValue = RgbValue {
    red: 134,
    green: 225,
    blue: 252,
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
        let width: usize = (*global.window_size.col_width).into();
        let height: usize = (*global.window_size.row_height).into();
        let state = &mut global.state;
        match event {
            InputEvent::Keyboard(KeyPress::Plain { key }) => match key {
                Key::Character('q') | Key::SpecialKey(SpecialKey::Esc) => {
                    return Ok(EventPropagation::ExitMainEventLoop);
                }
                Key::SpecialKey(SpecialKey::Left) | Key::SpecialKey(SpecialKey::BackTab) => {
                    apply_footer_action(state, FooterAction::PreviousDisk);
                }
                Key::SpecialKey(SpecialKey::Right) | Key::SpecialKey(SpecialKey::Tab) => {
                    apply_footer_action(state, FooterAction::NextDisk);
                }
                Key::SpecialKey(SpecialKey::Up) | Key::Character('k') => {
                    apply_footer_action(state, FooterAction::ScrollUp);
                }
                Key::SpecialKey(SpecialKey::Down) | Key::Character('j') => {
                    apply_footer_action(state, FooterAction::ScrollDown);
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
                        if let Some(action) = layout.control_at(x, y) {
                            apply_footer_action(state, action);
                        } else if let Some(index) = layout.tab_at(x, y) {
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
        ensure_selected_visible(state, width, height);
        clamp_content_scroll(state, width, height);
        Ok(EventPropagation::ConsumedRender)
    }

    fn app_handle_signal(
        &mut self,
        signal: &AppSignal,
        global: &mut GlobalData<Dashboard, AppSignal>,
        _components: &mut ComponentRegistryMap<Dashboard, AppSignal>,
        _focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        let width: usize = (*global.window_size.col_width).into();
        let height: usize = (*global.window_size.row_height).into();
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
                let previous_selection = state.selected;
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
                let preserved_selection = old_id
                    .as_ref()
                    .and_then(|id| state.disks.iter().position(|disk| disk.identity.id == *id));
                if preserved_selection.is_none() {
                    state.content_scroll = 0;
                }
                state.selected = preserved_selection
                    .unwrap_or(previous_selection)
                    .min(state.disks.len().saturating_sub(1));
                state.notice = if state.disks.is_empty() {
                    Some("Nenhum disco encontrado".into())
                } else {
                    None
                };
                clamp_tab_scroll(state, width, height);
                ensure_selected_visible(state, width, height);
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
                        let kind = effective_disk_kind(&disk.identity, &telemetry);
                        let score = score_health(kind, &telemetry);
                        if let Some(timestamp_unix_secs) = next_timestamp(&state.history, disk_id) {
                            let sample = HealthSample {
                                disk_id: disk.identity.id.clone(),
                                timestamp_unix_secs,
                                kind,
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
        let width: usize = (*global.window_size.col_width).into();
        let height: usize = (*global.window_size.row_height).into();
        clamp_tab_scroll(&mut global.state, width, height);
        ensure_selected_visible(&mut global.state, width, height);
        clamp_content_scroll(&mut global.state, width, height);
        let state = &global.state;
        let layout = Layout::new(width, height, state.disks.len(), state.scroll);
        let mut pipeline = RenderPipeline::default();
        let mut ops = RenderOpIRVec::new();
        ops += RenderOpCommon::SetBgColor(TuiColor::Rgb(TOKYO_BG));
        ops += RenderOpCommon::SetFgColor(TuiColor::Rgb(TOKYO_FG));
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
                TOKYO_FG,
                false,
                width,
                Some(TOKYO_BG),
            );
        }
        for y in 0..layout.quit.height.min(height) {
            fill_row(&mut ops, y, width, TOKYO_BG_DARK);
        }
        let title_x = usize::from(width >= 24);
        let full_title = format!("chkdsk v{}", env!("CARGO_PKG_VERSION"));
        let title_width = layout.quit.x.saturating_sub(title_x + 1);
        let title = if r3bl_tui::GCStringOwned::new(&full_title)
            .display_width()
            .as_usize()
            <= title_width
        {
            full_title
        } else {
            "chkdsk".to_owned()
        };
        paint_background(
            &mut ops,
            title_x,
            layout.quit.height / 2,
            &title,
            TOKYO_BLUE,
            true,
            title_width,
            Some(TOKYO_BG_DARK),
        );
        if layout.quit.visible() {
            paint_button(
                &mut ops,
                layout.quit,
                if layout.quit.width >= 6 { "Sair" } else { "×" },
                TOKYO_BG_DARK1,
                TOKYO_RED,
                true,
            );
        }
        for (position, area) in layout.tabs.iter().enumerate() {
            let index = layout.tab_indices[position];
            let disk = &state.disks[index];
            let mark = if index == state.selected { "> " } else { "" };
            // Disk identity belongs in the permanent tab; the model appears once below.
            let label = format!("{mark}Disco #{}", disk.identity.index);
            let (fg, bg) = if index == state.selected {
                (TOKYO_FG, TOKYO_BG_VISUAL)
            } else if state.hovered_tab == Some(index) {
                (TOKYO_FG, TOKYO_BG_HIGHLIGHT)
            } else {
                (TOKYO_FG_DARK, TOKYO_BG_DARK1)
            };
            paint_button(&mut ops, *area, &label, fg, bg, index == state.selected);
        }
        if height > layout.body_y {
            if let Some(disk) = state.disks.get(state.selected) {
                render_disk(
                    &mut ops,
                    disk,
                    &state.history,
                    layout.body_y,
                    width,
                    body_view_height(&layout, height),
                    state.content_scroll,
                    state.frame,
                );
            } else {
                paint_background(
                    &mut ops,
                    width.min(2).saturating_sub(1),
                    layout.body_y,
                    state.notice.as_deref().unwrap_or("Procurando discos…"),
                    TOKYO_FG_DARK,
                    false,
                    width.saturating_sub(2),
                    Some(TOKYO_BG_DARK1),
                );
            }
        }
        for control in &layout.controls {
            let color = match control.action {
                FooterAction::PreviousDisk | FooterAction::NextDisk => TOKYO_BLUE,
                FooterAction::ScrollUp | FooterAction::ScrollDown => TOKYO_CYAN,
            };
            paint_button(
                &mut ops,
                control.area,
                control.label,
                TOKYO_BG_DARK1,
                color,
                true,
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

fn apply_footer_action(state: &mut Dashboard, action: FooterAction) {
    match action {
        FooterAction::PreviousDisk => move_selection(state, false, 1),
        FooterAction::NextDisk => move_selection(state, true, 1),
        FooterAction::ScrollUp => {
            state.content_scroll = state.content_scroll.saturating_sub(1);
        }
        FooterAction::ScrollDown => {
            state.content_scroll = state.content_scroll.saturating_add(1);
        }
    }
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

fn clamp_tab_scroll(state: &mut Dashboard, width: usize, height: usize) {
    let disk_count = state.disks.len();
    if disk_count == 0 {
        state.scroll = 0;
        return;
    }
    let visible_count = Layout::new(width, height, disk_count, 0).tab_indices.len();
    let max_scroll = if visible_count == 0 {
        disk_count - 1
    } else {
        disk_count.saturating_sub(visible_count)
    };
    state.scroll = state.scroll.min(max_scroll);
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
    controls: Vec<FooterControl>,
    pub body_y: usize,
}

const TAB_BUTTON_WIDTH: usize = 18;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FooterAction {
    PreviousDisk,
    NextDisk,
    ScrollUp,
    ScrollDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FooterControl {
    area: HitArea,
    label: &'static str,
    action: FooterAction,
}

impl Layout {
    pub fn new(width: usize, height: usize, disks: usize, scroll: usize) -> Self {
        if width == 0 || height == 0 {
            return Self::default();
        }
        // Normal-sized buttons have one cell of vertical padding above and below
        // their centered label. Preserve a one-row escape hatch for tiny terminals.
        let button_height = if height >= 7 { 3 } else { 1 };
        let quit = if width >= 21 {
            HitArea {
                x: width - 8,
                y: 0,
                width: 7,
                height: button_height,
            }
        } else if width >= 18 {
            HitArea {
                x: width - 5,
                y: 0,
                width: 4,
                height: button_height,
            }
        } else {
            HitArea {
                x: width - 1,
                y: 0,
                width: 1,
                height: button_height,
            }
        };
        let mut tabs = Vec::new();
        let mut tab_indices = Vec::new();
        // Leave a quiet one-cell gutter around the horizontal tab strip when
        // the terminal can spare it; narrow terminals keep the full width.
        let side_margin = usize::from(width >= 24);
        let mut x = side_margin;
        let y = button_height;
        let start = scroll.min(disks.saturating_sub(1));
        if width >= 3 && height > 2 * button_height {
            for index in start..disks {
                let remaining = width.saturating_sub(x + side_margin);
                if remaining < TAB_BUTTON_WIDTH && !tabs.is_empty() {
                    break;
                }
                let available = TAB_BUTTON_WIDTH.min(remaining);
                if available == 0 {
                    break;
                }
                tabs.push(HitArea {
                    x,
                    y,
                    width: available,
                    height: button_height,
                });
                tab_indices.push(index);
                x += available + usize::from(width >= 24);
                if x >= width {
                    break;
                }
            }
        }
        let body_y = (2 * button_height).min(height);
        let controls = footer_controls(width, height);
        Self {
            tabs,
            tab_indices,
            quit,
            controls,
            body_y,
        }
    }
    pub fn tab_at(&self, x: usize, y: usize) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.contains(x, y))
            .and_then(|position| self.tab_indices.get(position).copied())
    }

    fn control_at(&self, x: usize, y: usize) -> Option<FooterAction> {
        self.controls
            .iter()
            .find(|control| control.area.contains(x, y))
            .map(|control| control.action)
    }
}

fn footer_controls(width: usize, height: usize) -> Vec<FooterControl> {
    // Compact screens reserve their height for the health summary. The same
    // mouse actions remain available through tab clicks and wheel scrolling.
    if width < 21 || height < 14 {
        return Vec::new();
    }
    let button_height = 3;
    let controls: &[(&str, FooterAction)] = if width >= 54 {
        &[
            ("← Discos", FooterAction::PreviousDisk),
            ("→ Discos", FooterAction::NextDisk),
            ("↑ Detalhes", FooterAction::ScrollUp),
            ("↓ Detalhes", FooterAction::ScrollDown),
        ]
    } else if width >= 39 {
        &[
            ("← Disco", FooterAction::PreviousDisk),
            ("→ Disco", FooterAction::NextDisk),
            ("↑ Info", FooterAction::ScrollUp),
            ("↓ Info", FooterAction::ScrollDown),
        ]
    } else if width >= 33 {
        &[
            ("←Dsc", FooterAction::PreviousDisk),
            ("→Dsc", FooterAction::NextDisk),
            ("↑Dados", FooterAction::ScrollUp),
            ("↓Dados", FooterAction::ScrollDown),
        ]
    } else {
        &[
            ("←D", FooterAction::PreviousDisk),
            ("→D", FooterAction::NextDisk),
            ("↑I", FooterAction::ScrollUp),
            ("↓I", FooterAction::ScrollDown),
        ]
    };
    let mut x = 1;
    let right_edge = width.saturating_sub(1);
    let mut result = controls
        .iter()
        .filter_map(|(label, action)| {
            let label_width = r3bl_tui::GCStringOwned::new(*label)
                .display_width()
                .as_usize();
            let button_width = label_width + 2;
            if x + button_width > right_edge {
                return None;
            }
            let control = FooterControl {
                area: HitArea {
                    x,
                    y: height - button_height,
                    width: button_width,
                    height: button_height,
                },
                label,
                action: *action,
            };
            x += button_width + 1;
            Some(control)
        })
        .collect::<Vec<_>>();
    if let (Some(first), Some(last)) = (result.first(), result.last()) {
        let occupied_width = last.area.x + last.area.width - first.area.x;
        let centered_start = width.saturating_sub(occupied_width) / 2;
        let shift = centered_start.saturating_sub(first.area.x);
        for control in &mut result {
            control.area.x += shift;
        }
    }
    result
}

fn body_view_height(layout: &Layout, height: usize) -> usize {
    let footer_height = layout
        .controls
        .first()
        .map_or(0, |control| control.area.height);
    height.saturating_sub(layout.body_y + footer_height)
}

fn disk_content_line_count(disk: &DiskView, width: usize, body_height: usize) -> usize {
    let Some(telemetry) = disk.telemetry.as_ref() else {
        return 5;
    };

    let kind = effective_disk_kind(&disk.identity, telemetry);
    let health = score_health(kind, telemetry);
    let large_score = body_height >= 9 && width >= 26;
    10 + usize::from(large_score) * 5 + health_detail_messages(&health, telemetry).len()
}

fn clamp_content_scroll(state: &mut Dashboard, width: usize, height: usize) {
    let layout = Layout::new(width, height, state.disks.len(), state.scroll);
    let body_height = body_view_height(&layout, height);
    let max_scroll = state
        .disks
        .get(state.selected)
        .map(|disk| disk_content_line_count(disk, width, body_height).saturating_sub(body_height))
        .unwrap_or_default();
    state.content_scroll = state.content_scroll.min(max_scroll);
}

fn centered_button_text(area: HitArea, label: &str) -> Option<(usize, usize, String)> {
    if !area.visible() {
        return None;
    }
    let inner_width = area.width.saturating_sub(2);
    let text = clipped(label, inner_width.max(usize::from(area.width > 0)));
    let text_width = r3bl_tui::GCStringOwned::new(&text)
        .display_width()
        .as_usize();
    let x = area.x + area.width.saturating_sub(text_width) / 2;
    let y = area.y + area.height / 2;
    Some((x, y, text))
}

fn paint_button(
    ops: &mut RenderOpIRVec,
    area: HitArea,
    label: &str,
    fg: RgbValue,
    bg: RgbValue,
    bold: bool,
) {
    let Some((text_x, text_y, text)) = centered_button_text(area, label) else {
        return;
    };
    for y in area.y..area.y + area.height {
        paint_background(
            ops,
            area.x,
            y,
            &" ".repeat(area.width),
            fg,
            bold,
            area.width,
            Some(bg),
        );
    }
    paint_background(
        ops,
        text_x,
        text_y,
        &text,
        fg,
        bold,
        area.x + area.width - text_x,
        Some(bg),
    );
}

fn fill_row(ops: &mut RenderOpIRVec, y: usize, width: usize, color: RgbValue) {
    if width > 0 {
        paint_background(
            ops,
            0,
            y,
            &" ".repeat(width),
            TOKYO_FG,
            false,
            width,
            Some(color),
        );
    }
}

fn paint_table_text(
    ops: &mut RenderOpIRVec,
    y: usize,
    label: &str,
    value: &str,
    width: usize,
    background: RgbValue,
) {
    const LABEL_WIDTH: usize = 12;
    const VALUE_X: usize = 15;
    paint_background(
        ops,
        1.min(width.saturating_sub(1)),
        y,
        &pad_to_columns(label, LABEL_WIDTH),
        TOKYO_FG_DARK,
        false,
        LABEL_WIDTH.min(width.saturating_sub(1)),
        Some(background),
    );
    if VALUE_X < width {
        paint_background(
            ops,
            VALUE_X,
            y,
            value,
            TOKYO_FG,
            false,
            width.saturating_sub(VALUE_X),
            Some(background),
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
    let mut row = |ops: &mut RenderOpIRVec, text: &str, fg: RgbValue, bg: RgbValue, bold: bool| {
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
            line += 1;
            Some(y)
        } else {
            line += 1;
            None
        }
    };
    let model = disk
        .identity
        .model
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Modelo desconhecido");
    row(ops, model, TOKYO_FG, TOKYO_BG_HIGHLIGHT, true);

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
            TOKYO_CYAN
        } else {
            TOKYO_RED
        };
        row(ops, message, color, TOKYO_BG_DARK1, true);
        let kind = match disk.identity.kind {
            DiskKind::Hdd => "HDD",
            DiskKind::Ssd => "SSD",
            DiskKind::Nvme => "NVMe",
            DiskKind::Unknown => "Desconhecido",
        };
        let mut stripe = false;
        for (label, value) in [
            ("TIPO", kind.to_owned()),
            (
                "INTERFACE",
                disk.identity
                    .bus
                    .clone()
                    .unwrap_or_else(|| "indisponível".into()),
            ),
            (
                "CAPACIDADE",
                disk.identity
                    .capacity_bytes
                    .map(format_bytes)
                    .unwrap_or_else(|| "indisponível".into()),
            ),
        ] {
            let background = if stripe { TOKYO_BG } else { TOKYO_BG_DARK1 };
            if let Some(y) = row(ops, "", TOKYO_FG, background, false) {
                paint_table_text(ops, y, label, &value, width, background);
            }
            stripe = !stripe;
        }
        return;
    };
    let kind = effective_disk_kind(&disk.identity, telemetry);
    let health = score_health(kind, telemetry);
    let (label, color) = health_presentation(&health);
    let nominal_attention = health.outcome == HealthOutcome::Pass
        && health
            .warnings
            .contains(&HealthWarningReason::EnduranceExhausted);
    let basic_smart = is_basic_smart_pass(&health);
    let large_score = available_height >= 9 && width >= 26;
    let headline = if large_score || health.outcome == HealthOutcome::Unknown {
        if label.starts_with("SAÚDE") {
            label.to_owned()
        } else {
            format!("SAÚDE · {label}")
        }
    } else if nominal_attention {
        format!("NOMINAL · {}%", health.score)
    } else if basic_smart {
        format!("SAÚDE · SMART BÁSICO · {}%", health.score)
    } else {
        format!("{label} · {}%", health.score)
    };
    row(ops, &headline, color, TOKYO_BG_DARK1, true);

    if large_score {
        let score_text = if health.outcome == HealthOutcome::Unknown {
            "N/A".to_owned()
        } else {
            format!("{}%", health.score)
        };
        for score_row in ascii_score_lines(&score_text) {
            row(ops, &score_row, color, TOKYO_BG, true);
        }
    }
    let eta = if kind == DiskKind::Hdd {
        if width < 40 {
            "ETA não aplicável a HDD".to_owned()
        } else {
            "ETA não aplicável · vida útil de HDD".to_owned()
        }
    } else if kind == DiskKind::Unknown {
        if width < 40 {
            "Sem ETA: tipo desconhecido".to_owned()
        } else {
            "ETA indisponível · tipo sem previsão".to_owned()
        }
    } else if width < 40 && history.estimate_endurance(&disk.identity.id).is_none() {
        "ETA: requer histórico".to_owned()
    } else {
        history
            .estimate_endurance(&disk.identity.id)
            .map(|estimate| {
                format!(
                    "ETA restante  {}",
                    format_endurance_days(estimate.remaining_seconds)
                )
            })
            .unwrap_or_else(|| "ETA indisponível · requer histórico de desgaste".into())
    };
    // Health status and score are the focus; keep the long-range forecast useful
    // but visually secondary.
    row(ops, &eta, TOKYO_FG_DARK, TOKYO_BG_DARK, false);

    let full_status = telemetry_status(telemetry);
    let status = if basic_smart {
        if width < 40 {
            "SMART básico: baixa confiança"
        } else {
            "SMART básico: escore de baixa confiança; sem mapeamento detalhado"
        }
    } else if width >= 40 {
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
            TOKYO_RED
        } else if health.outcome == HealthOutcome::Unknown
            || full_status.contains("parcial")
            || basic_smart
            || !health.warnings.is_empty()
        {
            TOKYO_YELLOW
        } else {
            TOKYO_GREEN
        };
    row(ops, status, status_color, TOKYO_BG_DARK1, false);
    for (message, color) in health_detail_messages(&health, telemetry) {
        row(ops, &message, color, TOKYO_BG_DARK1, color == TOKYO_RED);
    }
    let kind = match kind {
        DiskKind::Hdd => "HDD",
        DiskKind::Ssd => "SSD",
        DiskKind::Nvme => "NVMe",
        DiskKind::Unknown => "Desconhecido",
    };
    let bus = disk
        .identity
        .bus
        .as_deref()
        .unwrap_or("interface indisponível");
    let capacity = disk
        .identity
        .capacity_bytes
        .map(format_bytes)
        .unwrap_or_else(|| "indisponível".into());
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
        .map(compact_number)
        .unwrap_or_else(|| "—".into());
    let mut stripe = false;
    for (label, value) in [
        ("TIPO", kind.to_owned()),
        ("INTERFACE", bus.to_owned()),
        ("CAPACIDADE", capacity),
        ("DESGASTE", wear),
        ("TEMPERATURA", temperature),
        ("LIGADO", hours),
    ] {
        let background = if stripe { TOKYO_BG } else { TOKYO_BG_DARK1 };
        if let Some(y) = row(ops, "", TOKYO_FG, background, false) {
            paint_table_text(ops, y, label, &value, width, background);
        }
        stripe = !stripe;
    }
    let _ = frame;
}

/// A verified ATA SSD profile with measured wear can fill a missing media kind.
/// Never override an explicit HDD classification or infer SSD from an opaque ID.
fn effective_disk_kind(identity: &DiskIdentity, telemetry: &Telemetry) -> DiskKind {
    if identity.kind == DiskKind::Unknown
        && telemetry.endurance_percent.is_some()
        && matches!(telemetry.ata, QueryState::Success(_))
        && crate::ata::profile(identity.model.as_deref())
            .is_some_and(|profile| telemetry.ata_profile.as_deref() == Some(profile.family))
    {
        DiskKind::Ssd
    } else {
        identity.kind
    }
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
        HealthOutcome::Unknown => ("SAÚDE INDISPONÍVEL", TOKYO_YELLOW),
        HealthOutcome::Fail => ("FALHA DETECTADA", TOKYO_RED),
        HealthOutcome::Pass
            if health
                .warnings
                .contains(&HealthWarningReason::EnduranceExhausted) =>
        {
            ("VIDA NOMINAL ESGOTADA", TOKYO_YELLOW)
        }
        HealthOutcome::Pass if !health.warnings.is_empty() => {
            ("ATENÇÃO · VER AVISOS", TOKYO_YELLOW)
        }
        HealthOutcome::Pass if health.score <= 50 => ("ATENÇÃO · SAÚDE REDUZIDA", TOKYO_YELLOW),
        HealthOutcome::Pass if is_basic_smart_pass(health) => {
            ("SAÚDE · LEITURA SMART BÁSICA", TOKYO_YELLOW)
        }
        HealthOutcome::Pass => ("SAUDÁVEL", TOKYO_GREEN),
    }
}

fn health_detail_messages(
    health: &crate::health::HealthScore,
    telemetry: &Telemetry,
) -> Vec<(String, RgbValue)> {
    let mut messages = Vec::new();
    if health.outcome == HealthOutcome::Fail {
        if let QueryState::Success(nvme) = &telemetry.nvme {
            let spare_below_threshold = if let (Some(spare), Some(threshold)) = (
                nvme.available_spare_percent,
                nvme.available_spare_threshold_percent,
            ) && spare < threshold
            {
                messages.push((
                    format!("Reserva NVMe abaixo do limite ({spare}% < {threshold}%)"),
                    TOKYO_RED,
                ));
                true
            } else {
                false
            };
            for (bit, message) in [
                (0b0000_0001, "Reserva NVMe em nível crítico"),
                (0b0000_0100, "Confiabilidade NVMe comprometida"),
                (0b0000_1000, "NVMe em modo somente leitura"),
            ] {
                if nvme.critical_warning & bit != 0
                    && !(bit == 0b0000_0001 && spare_below_threshold)
                {
                    messages.push((message.into(), TOKYO_RED));
                }
            }
        }
        if let QueryState::Success(ata) = &telemetry.ata {
            for attribute in &ata.attributes {
                if (1..=254).contains(&attribute.current)
                    && attribute.flags & 1 != 0
                    && let Some(threshold) = attribute.threshold.filter(|value| *value > 0)
                    && attribute.current <= threshold
                {
                    messages.push((
                        format!(
                            "Falha SMART iminente: atributo ATA {} atingiu o limite ({}/{})",
                            attribute.id, attribute.current, threshold
                        ),
                        TOKYO_RED,
                    ));
                }
            }
        }
        if messages.is_empty() && matches!(telemetry.overall_failure, QueryState::Success(true)) {
            messages.push(("Falha geral reportada pelo dispositivo".into(), TOKYO_RED));
        }
        if messages.is_empty() {
            messages.push(("Falha crítica de saúde confirmada".into(), TOKYO_RED));
        }
    }

    for warning in &health.warnings {
        let warning_messages = match warning {
            HealthWarningReason::AtaThresholdMargin => ata_threshold_messages(telemetry, true)
                .unwrap_or_else(|| vec!["SMART ATA próximo do limite declarado".into()]),
            HealthWarningReason::AtaThresholdReached => ata_threshold_messages(telemetry, false)
                .unwrap_or_else(|| vec!["SMART ATA atingiu o limite declarado".into()]),
            HealthWarningReason::EnduranceExhausted => {
                vec!["Vida útil nominal do dispositivo esgotada".into()]
            }
            HealthWarningReason::NvmeMediaErrors => vec![match &telemetry.nvme {
                QueryState::Success(nvme) => nvme
                    .media_errors
                    .filter(|count| *count > 0)
                    .map(|count| format!("Erros de mídia NVMe registrados: {count}"))
                    .unwrap_or_else(|| "Erros de mídia NVMe registrados".into()),
                QueryState::NotQueried | QueryState::Unsupported | QueryState::Failed { .. } => {
                    "Erros de mídia NVMe registrados".into()
                }
            }],
            HealthWarningReason::NvmeTemperatureThreshold => {
                vec!["Temperatura NVMe fora dos limites".into()]
            }
            HealthWarningReason::NvmeVolatileMemoryBackup => {
                vec!["Backup da memória volátil NVMe degradado".into()]
            }
            HealthWarningReason::NvmePersistentMemoryRegion => {
                vec!["Região de memória persistente NVMe degradada ou somente leitura".into()]
            }
        };
        messages.extend(
            warning_messages
                .into_iter()
                .map(|message| (message, TOKYO_YELLOW)),
        );
    }
    messages
}

fn ata_threshold_messages(telemetry: &Telemetry, near: bool) -> Option<Vec<String>> {
    let QueryState::Success(ata) = &telemetry.ata else {
        return None;
    };
    let messages = ata
        .attributes
        .iter()
        .filter_map(|attribute| {
            if !(1..=254).contains(&attribute.current) {
                return None;
            }
            let threshold = attribute.threshold.filter(|value| *value > 0)?;
            let matches = if near {
                attribute.current > threshold
                    && attribute.current <= threshold.saturating_add(10)
                    && attribute.flags & 1 == 0
            } else {
                attribute.current <= threshold && attribute.flags & 1 == 0
            };
            matches.then(|| {
                let description = if near {
                    "próximo do limite"
                } else {
                    "atingiu o limite"
                };
                format!(
                    "SMART ATA {description}: atributo {} ({}/{threshold})",
                    attribute.id, attribute.current
                )
            })
        })
        .collect::<Vec<_>>();
    (!messages.is_empty()).then_some(messages)
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
    let background = background.unwrap_or(TOKYO_BG);
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
    *ops += RenderOpCommon::SetBgColor(TuiColor::Rgb(TOKYO_BG));
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

    fn dispatch_signal(
        app: &mut DashboardApp,
        global: &mut GlobalData<Dashboard, AppSignal>,
        signal: AppSignal,
    ) -> EventPropagation {
        app.app_handle_signal(
            &signal,
            global,
            &mut ComponentRegistryMap::default(),
            &mut HasFocus::default(),
        )
        .unwrap()
    }

    fn rendered_text(
        app: &mut DashboardApp,
        global: &mut GlobalData<Dashboard, AppSignal>,
    ) -> String {
        let width: usize = (*global.window_size.col_width).into();
        let height: usize = (*global.window_size.row_height).into();
        let pipeline = app
            .app_render(
                global,
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
        buffer
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
            .join("\n")
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

    fn is_tokyo_moon_color(color: TuiColor) -> bool {
        matches!(color, TuiColor::Rgb(value) if [
            TOKYO_BG, TOKYO_BG_DARK, TOKYO_BG_DARK1, TOKYO_BG_HIGHLIGHT, TOKYO_BG_VISUAL,
            TOKYO_FG, TOKYO_FG_DARK, TOKYO_BLUE, TOKYO_GREEN, TOKYO_RED, TOKYO_YELLOW,
            TOKYO_ORANGE, TOKYO_PURPLE, TOKYO_CYAN,
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

    #[test]
    fn tokyo_night_moon_tokens_match_the_pinned_default_palette() {
        let actual = [
            TOKYO_BG,
            TOKYO_BG_DARK,
            TOKYO_BG_DARK1,
            TOKYO_BG_HIGHLIGHT,
            TOKYO_BG_VISUAL,
            TOKYO_FG,
            TOKYO_FG_DARK,
            TOKYO_BLUE,
            TOKYO_GREEN,
            TOKYO_ORANGE,
            TOKYO_YELLOW,
            TOKYO_RED,
            TOKYO_PURPLE,
            TOKYO_CYAN,
        ]
        .map(|color| rgb_hex(TuiColor::Rgb(color)));
        assert_eq!(
            actual,
            [
                "#222436", "#1e2030", "#191b29", "#2f334d", "#2d3f76", "#c8d3f5", "#828bb8",
                "#82aaff", "#c3e88d", "#ff966c", "#ffc777", "#ff757f", "#fca7ea", "#86e1fc",
            ]
        );
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
    fn rendered_cells_cover_viewport_with_explicit_tokyo_moon_truecolor_and_optional_proof() {
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
            "<!doctype html><meta charset=\"utf-8\"><title>chkdsk · Tokyo Night Moon</title><style>body{margin:24px;background:#222436;color:#C8D3F5;font:14px system-ui,sans-serif}h1{font-size:18px;font-weight:600;margin:0 0 20px}section{margin:0 0 30px}.case{color:#828BB8;margin:8px 0 5px}.viewport{overflow:auto;display:inline-block;background:#222436;padding:2px}.row{display:grid;grid-auto-rows:16px}.cell{display:block;width:8px;height:16px;padding:0;margin:0;overflow:hidden;font-family:\"Cascadia Mono\",\"Cascadia Code\",Consolas,monospace;font-size:13px;line-height:16px;letter-spacing:0;font-kerning:none;white-space:pre}.cell.wide{grid-column:span 2;width:16px}</style><h1>chkdsk · Tokyo Night Moon · renderização exata de células</h1>",
        );
        let output_dir = std::env::var_os("CHKDSK_UI_PROOF_DIR").map(PathBuf::from);
        for (fixture_name, kind, model, bus, telemetry) in fixtures {
            for (width, height) in [(100, 28), (70, 20), (30, 10), (12, 4), (1, 1)] {
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
                        assert!(is_tokyo_moon_color(style.color_fg.unwrap()));
                        assert!(is_tokyo_moon_color(style.color_bg.unwrap()));
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
    fn render_keeps_selected_disk_tab_visible_after_terminal_resize() {
        let mut app = DashboardApp {
            started: true,
            ..Default::default()
        };
        let mut global = test_global_data(
            Dashboard {
                disks: (0..9)
                    .map(|index| DiskView {
                        identity: identity(&format!("disk-{index}"), index),
                        telemetry: None,
                        querying: false,
                        computing: false,
                        pending_telemetry: None,
                        error_message: None,
                    })
                    .collect(),
                selected: 8,
                ..Default::default()
            },
            100,
            28,
        );
        for (width, height) in [(100, 28), (44, 28), (22, 10), (100, 28)] {
            global.window_size = r3bl_tui::Size::new((
                r3bl_tui::ColWidth::new(width),
                r3bl_tui::RowHeight::new(height),
            ));
            let rendered = rendered_text(&mut app, &mut global);
            let layout = Layout::new(width, height, global.state.disks.len(), global.state.scroll);
            assert_eq!(global.state.selected, 8);
            if width == 100 {
                assert_eq!(global.state.scroll, 4);
                assert_eq!(layout.tab_indices, vec![4, 5, 6, 7, 8]);
            }
            assert!(
                layout.tab_indices.contains(&8),
                "{width}x{height}: {layout:?}"
            );
            assert!(
                rendered
                    .lines()
                    .nth(layout.tabs[0].y + layout.tabs[0].height / 2)
                    .unwrap()
                    .contains("> Disco #8"),
                "{width}x{height}: {rendered}"
            );
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
        assert_eq!(visible.tab_indices, vec![0, 1, 2, 3, 4]);
        assert_eq!(visible.tab_at(20, 4), Some(1));
        assert_eq!(visible.tab_at(37, 4), Some(1));
        assert_eq!(visible.tab_at(38, 4), None);
        assert_eq!(visible.tab_at(39, 4), Some(2));
        state.selected = 5;
        ensure_selected_visible(&mut state, 100, 28);
        let shifted = Layout::new(100, 28, state.disks.len(), state.scroll);
        assert_eq!(shifted.tab_indices, vec![1, 2, 3, 4, 5]);
        assert_eq!(shifted.tab_at(1, 4), Some(1));
        assert_eq!(shifted.tab_at(1 + 4 * 19, 4), Some(5));

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
                mouse_event(MouseInputKind::MouseMove, 20, 4)
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
            matches!(buffer[4][24], r3bl_tui::PixelChar::PlainText { style, .. } if style.color_bg == Some(TuiColor::Rgb(TOKYO_BG_HIGHLIGHT)))
        );
    }

    #[test]
    fn discovery_reorder_and_removal_preserve_shared_selection_and_reveal_its_tab() {
        let disks = (0..9)
            .map(|index| {
                let mut disk_identity = identity(&format!("disk-{index}"), index);
                let telemetry = if index == 5 {
                    disk_identity.kind = DiskKind::Hdd;
                    Some(Telemetry {
                        overall_failure: QueryState::Success(false),
                        ..Default::default()
                    })
                } else {
                    None
                };
                DiskView {
                    identity: disk_identity,
                    telemetry,
                    querying: false,
                    computing: false,
                    pending_telemetry: None,
                    error_message: None,
                }
            })
            .collect();
        let mut app = DashboardApp {
            started: true,
            ..Default::default()
        };
        let mut global = test_global_data(
            Dashboard {
                disks,
                selected: 5,
                scroll: 4,
                ..Default::default()
            },
            100,
            10,
        );

        assert_eq!(Layout::new(100, 10, 9, 4).tab_at(20, 4), Some(5));
        dispatch_input(
            &mut app,
            &mut global,
            mouse_event(MouseInputKind::MouseDown(Button::Left), 20, 4),
        );
        assert_eq!(
            global.state.disks[global.state.selected].identity.id,
            "disk-5"
        );
        for _ in 0..2 {
            dispatch_input(
                &mut app,
                &mut global,
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Down),
                }),
            );
        }
        assert_eq!(global.state.content_scroll, 2);

        let reordered = [5, 0, 1, 2, 3, 4, 6, 7, 8]
            .into_iter()
            .map(|index| {
                let mut disk_identity = identity(&format!("disk-{index}"), index);
                if index == 5 {
                    disk_identity.kind = DiskKind::Hdd;
                }
                disk_identity
            })
            .collect();
        assert!(matches!(
            dispatch_signal(
                &mut app,
                &mut global,
                AppSignal::Discovered {
                    generation: 1,
                    result: Ok(reordered),
                },
            ),
            EventPropagation::ConsumedRender
        ));
        assert_eq!(
            global.state.disks[global.state.selected].identity.id,
            "disk-5"
        );
        assert_eq!(global.state.selected, 0);
        assert_eq!(global.state.scroll, 0);
        assert_eq!(global.state.content_scroll, 2);
        assert!(
            Layout::new(100, 10, 9, global.state.scroll)
                .tab_indices
                .contains(&global.state.selected)
        );
        assert!(
            rendered_text(&mut app, &mut global)
                .lines()
                .nth(1)
                .unwrap()
                .contains("> Disco #5")
        );

        dispatch_input(
            &mut app,
            &mut global,
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Right),
            }),
        );
        assert_eq!(
            global.state.disks[global.state.selected].identity.id,
            "disk-0"
        );

        global.state.disks[global.state.selected].identity.kind = DiskKind::Hdd;
        global.state.disks[global.state.selected].telemetry = Some(Telemetry {
            overall_failure: QueryState::Success(false),
            ..Default::default()
        });
        for _ in 0..2 {
            dispatch_input(
                &mut app,
                &mut global,
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Down),
                }),
            );
        }
        assert_eq!(global.state.content_scroll, 2);

        global.state.scroll = 4;
        let removed_selection = [5, 1, 2, 3, 4, 6, 7, 8]
            .into_iter()
            .map(|index| {
                let mut disk_identity = identity(&format!("disk-{index}"), index);
                if index == 5 {
                    disk_identity.kind = DiskKind::Hdd;
                }
                disk_identity
            })
            .collect();
        dispatch_signal(
            &mut app,
            &mut global,
            AppSignal::Discovered {
                generation: 2,
                result: Ok(removed_selection),
            },
        );
        assert_eq!(global.state.selected, 1);
        assert_eq!(
            global.state.disks[global.state.selected].identity.id,
            "disk-1"
        );
        assert_eq!(global.state.scroll, 1);
        assert_eq!(global.state.content_scroll, 0);
        assert!(
            Layout::new(100, 10, 8, global.state.scroll)
                .tab_indices
                .contains(&global.state.selected)
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
            warnings: vec![HealthWarningReason::EnduranceExhausted],
        };
        assert_eq!(
            health_presentation(&nominal),
            ("VIDA NOMINAL ESGOTADA", TOKYO_YELLOW)
        );
        let coarse = crate::health::HealthScore {
            score: 100,
            outcome: HealthOutcome::Pass,
            confidence: 0.1,
            provenance: vec!["device-reported no threshold failure (coarse only)".into()],
            warnings: vec![],
        };
        assert_eq!(
            health_presentation(&coarse),
            ("SAÚDE · LEITURA SMART BÁSICA", TOKYO_YELLOW)
        );
        let healthy = crate::health::HealthScore {
            score: 88,
            outcome: HealthOutcome::Pass,
            confidence: 0.8,
            provenance: vec!["NVMe SMART/Health log".into()],
            warnings: vec![],
        };
        assert_eq!(health_presentation(&healthy), ("SAUDÁVEL", TOKYO_GREEN));
        let low_without_endurance_warning = crate::health::HealthScore {
            score: 20,
            outcome: HealthOutcome::Pass,
            confidence: 0.8,
            provenance: vec!["Other non-failure evidence".into()],
            warnings: vec![],
        };
        assert_eq!(
            health_presentation(&low_without_endurance_warning),
            ("ATENÇÃO · SAÚDE REDUZIDA", TOKYO_YELLOW)
        );
        let warning_at_70 = crate::health::HealthScore {
            score: 70,
            outcome: HealthOutcome::Pass,
            confidence: 0.8,
            provenance: vec!["NVMe SMART/Health log".into()],
            warnings: vec![HealthWarningReason::NvmeMediaErrors],
        };
        assert_eq!(
            health_presentation(&warning_at_70),
            ("ATENÇÃO · VER AVISOS", TOKYO_YELLOW)
        );
        let failed = crate::health::HealthScore {
            score: 20,
            outcome: HealthOutcome::Fail,
            confidence: 1.0,
            provenance: vec!["device-reported overall failure".into()],
            warnings: vec![],
        };
        assert_eq!(health_presentation(&failed), ("FALHA DETECTADA", TOKYO_RED));
    }

    #[test]
    fn health_warning_reasons_render_pt_br_yellow_and_confirmed_failures_red() {
        let telemetry = Telemetry {
            nvme: QueryState::Success(crate::model::NvmeHealth {
                critical_warning: 0b0011_0010,
                media_errors: Some(42),
                ..Default::default()
            }),
            ata: QueryState::Success(crate::model::AtaHealth {
                attributes: vec![
                    crate::model::AtaAttribute {
                        id: 9,
                        flags: 0,
                        current: 106,
                        worst: 100,
                        threshold: Some(100),
                        raw: [0; 6],
                    },
                    crate::model::AtaAttribute {
                        id: 194,
                        flags: 0,
                        current: 108,
                        worst: 100,
                        threshold: Some(100),
                        raw: [0; 6],
                    },
                    crate::model::AtaAttribute {
                        id: 197,
                        flags: 0,
                        current: 99,
                        worst: 99,
                        threshold: Some(100),
                        raw: [0; 6],
                    },
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        let warnings = crate::health::HealthScore {
            score: 70,
            outcome: HealthOutcome::Pass,
            confidence: 0.8,
            provenance: vec![],
            warnings: vec![
                HealthWarningReason::AtaThresholdMargin,
                HealthWarningReason::AtaThresholdReached,
                HealthWarningReason::NvmeMediaErrors,
                HealthWarningReason::NvmeTemperatureThreshold,
                HealthWarningReason::NvmeVolatileMemoryBackup,
                HealthWarningReason::NvmePersistentMemoryRegion,
            ],
        };
        let details = health_detail_messages(&warnings, &telemetry);
        let rendered = details
            .iter()
            .map(|(message, _)| message.as_str())
            .collect::<Vec<_>>();
        assert!(rendered.contains(&"SMART ATA próximo do limite: atributo 9 (106/100)"));
        assert!(rendered.contains(&"SMART ATA próximo do limite: atributo 194 (108/100)"));
        assert!(rendered.contains(&"SMART ATA atingiu o limite: atributo 197 (99/100)"));
        assert!(rendered.contains(&"Erros de mídia NVMe registrados: 42"));
        assert!(rendered.contains(&"Temperatura NVMe fora dos limites"));
        assert!(rendered.contains(&"Backup da memória volátil NVMe degradado"));
        assert!(
            rendered.contains(&"Região de memória persistente NVMe degradada ou somente leitura")
        );
        assert!(details.iter().all(|(_, color)| *color == TOKYO_YELLOW));

        let critical_telemetry = Telemetry {
            overall_failure: QueryState::Success(true),
            nvme: QueryState::Success(crate::model::NvmeHealth {
                critical_warning: 0b0000_1101,
                ..Default::default()
            }),
            ata: QueryState::Success(crate::model::AtaHealth {
                attributes: vec![crate::model::AtaAttribute {
                    id: 5,
                    flags: 1,
                    current: 10,
                    worst: 10,
                    threshold: Some(20),
                    raw: [0; 6],
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let critical = crate::health::HealthScore {
            score: 10,
            outcome: HealthOutcome::Fail,
            confidence: 1.0,
            provenance: vec![],
            warnings: vec![],
        };
        let details = health_detail_messages(&critical, &critical_telemetry);
        assert!(details.contains(&("Reserva NVMe em nível crítico".into(), TOKYO_RED)));
        assert!(details.contains(&("Confiabilidade NVMe comprometida".into(), TOKYO_RED)));
        assert!(details.contains(&("NVMe em modo somente leitura".into(), TOKYO_RED)));
        assert!(details.contains(&(
            "Falha SMART iminente: atributo ATA 5 atingiu o limite (10/20)".into(),
            TOKYO_RED
        )));
        let overall_failure = Telemetry {
            overall_failure: QueryState::Success(true),
            ..Default::default()
        };
        assert!(
            health_detail_messages(&critical, &overall_failure)
                .contains(&("Falha geral reportada pelo dispositivo".into(), TOKYO_RED))
        );
    }

    #[test]
    fn failure_details_match_validated_health_boundaries() {
        for warning in [1, 4] {
            let telemetry = Telemetry {
                nvme: QueryState::Success(crate::model::NvmeHealth {
                    available_spare_percent: Some(10),
                    available_spare_threshold_percent: Some(10),
                    critical_warning: warning,
                    ..Default::default()
                }),
                ..Default::default()
            };
            let health = score_health(DiskKind::Nvme, &telemetry);
            let details = health_detail_messages(&health, &telemetry);
            assert!(!details.iter().any(|(text, _)| text.contains("abaixo")));
            let cause = if warning == 1 {
                "Reserva NVMe em nível crítico"
            } else {
                "Confiabilidade NVMe comprometida"
            };
            assert!(details.contains(&(cause.into(), TOKYO_RED)));
        }
        for current in [0, 255] {
            let telemetry = Telemetry {
                overall_failure: QueryState::Success(true),
                ata: QueryState::Success(crate::model::AtaHealth {
                    attributes: vec![crate::model::AtaAttribute {
                        id: 5,
                        flags: 1,
                        current,
                        worst: current,
                        threshold: Some(10),
                        raw: [0; 6],
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            };
            let health = score_health(DiskKind::Hdd, &telemetry);
            let details = health_detail_messages(&health, &telemetry);
            assert_eq!(
                details,
                vec![("Falha geral reportada pelo dispositivo".into(), TOKYO_RED)]
            );
            assert!(ata_threshold_messages(&telemetry, false).is_none());
        }
    }

    #[test]
    fn verified_ssd_profile_restores_unknown_kind_history_and_eta() {
        let mut disk_identity = identity("profile-ssd", 0);
        disk_identity.model = Some("CT500MX500SSD1".into());
        let mut telemetry = Telemetry::default();
        telemetry.apply_ata(
            crate::model::AtaHealth {
                attributes: vec![crate::model::AtaAttribute {
                    id: 202,
                    flags: 0,
                    current: 89,
                    worst: 89,
                    threshold: None,
                    raw: [11, 0, 0, 0, 0, 0],
                }],
                ..Default::default()
            },
            disk_identity.model.as_deref(),
        );
        assert_eq!(
            effective_disk_kind(&disk_identity, &telemetry),
            DiskKind::Ssd
        );
        let mut app = DashboardApp {
            started: true,
            ..Default::default()
        };
        let mut global = test_global_data(
            Dashboard {
                disks: vec![DiskView {
                    identity: disk_identity.clone(),
                    telemetry: None,
                    querying: false,
                    computing: true,
                    pending_telemetry: Some(telemetry.clone()),
                    error_message: None,
                }],
                ..Default::default()
            },
            100,
            28,
        );
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        global
            .state
            .history
            .record(HealthSample {
                disk_id: disk_identity.id.clone(),
                timestamp_unix_secs: now - 86_400,
                kind: DiskKind::Ssd,
                endurance_percent: Some(10.0),
                power_on_hours: None,
                score: None,
            })
            .unwrap();
        dispatch_signal(
            &mut app,
            &mut global,
            AppSignal::ComputeFinished {
                generation: 0,
                disk_id: disk_identity.id.clone(),
            },
        );
        assert_eq!(
            global.state.history.samples.last().unwrap().kind,
            DiskKind::Ssd
        );
        assert!(
            global
                .state
                .history
                .estimate_endurance(&disk_identity.id)
                .is_some()
        );
        assert!(rendered_text(&mut app, &mut global).contains("ETA restante"));
        disk_identity.kind = DiskKind::Hdd;
        assert_eq!(
            effective_disk_kind(&disk_identity, &telemetry),
            DiskKind::Hdd
        );
        disk_identity.kind = DiskKind::Unknown;
        disk_identity.model = Some("Unknown SSD".into());
        assert_eq!(
            effective_disk_kind(&disk_identity, &telemetry),
            DiskKind::Unknown
        );
        disk_identity.model = Some("CT500MX500SSD1".into());
        telemetry.ata_profile = Some("Crucial BX500".into());
        assert_eq!(
            effective_disk_kind(&disk_identity, &telemetry),
            DiskKind::Unknown
        );
        telemetry.ata_profile = Some("Crucial MX500".into());
        telemetry.endurance_percent = None;
        assert_eq!(
            effective_disk_kind(&disk_identity, &telemetry),
            DiskKind::Unknown
        );
    }

    #[test]
    fn layouts_and_tab_hits_stay_inside_viewport_without_overlap() {
        for (width, height) in [(0, 0), (1, 1), (4, 2), (12, 6), (80, 24)] {
            let layout = Layout::new(width, height, 100, 20);
            let areas = layout
                .tabs
                .iter()
                .copied()
                .chain(layout.controls.iter().map(|control| control.area))
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
    fn compact_footer_reserves_space_for_the_health_summary() {
        let layout = Layout::new(30, 10, 2, 0);
        assert!(layout.controls.is_empty());
        assert_eq!(layout.quit.height, 3);
        assert_eq!(layout.tabs.len(), 1);
        assert_eq!(layout.tabs[0].height, 3);
        assert_eq!(body_view_height(&layout, 10), 4);
    }

    #[test]
    fn standard_buttons_are_padded_centered_and_have_full_hitboxes() {
        for (width, height) in [(100, 28), (70, 20), (30, 10)] {
            let layout = Layout::new(width, height, 5, 0);
            assert_eq!(layout.quit.height, 3, "{width}x{height}");
            assert!(!layout.tabs.is_empty(), "{width}x{height}");
            assert!(layout.tabs.iter().all(|tab| tab.height == 3));
            assert!(
                layout
                    .tabs
                    .iter()
                    .zip(layout.tabs.iter().skip(1))
                    .all(|(left, right)| right.x == left.x + left.width + 1)
            );

            let mut buttons = vec![(layout.quit, "Sair".to_owned())];
            buttons.extend(layout.tabs.iter().enumerate().map(|(position, area)| {
                let index = layout.tab_indices[position];
                let marker = if index == 0 { "> " } else { "" };
                (*area, format!("{marker}Disco #{index}"))
            }));
            buttons.extend(
                layout
                    .controls
                    .iter()
                    .map(|control| (control.area, control.label.to_owned())),
            );

            for (area, label) in buttons {
                assert!(area.width >= 3, "{width}x{height}: {area:?}");
                let (text_x, text_y, text) = centered_button_text(area, &label).unwrap();
                let text_width = r3bl_tui::GCStringOwned::new(&text)
                    .display_width()
                    .as_usize();
                assert_eq!(text_y, area.y + 1, "{width}x{height}: {area:?}");
                assert_eq!(
                    text_x,
                    area.x + (area.width - text_width) / 2,
                    "{width}x{height}: {area:?}"
                );
                assert!(text_x >= area.x + 1, "{width}x{height}: {area:?}");
                assert!(
                    text_x + text_width <= area.x + area.width - 1,
                    "{width}x{height}: {area:?}"
                );
                for y in area.y..area.y + area.height {
                    for x in area.x..area.x + area.width {
                        assert!(area.contains(x, y), "{width}x{height}: {area:?}");
                    }
                }
            }
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
        assert!(shown.contains("CAPACIDADE"));
        assert!(shown.contains("indisponível"));
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
        assert_eq!(global.state.content_scroll, 0);

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
    fn footer_button_rectangles_dispatch_their_matching_keyboard_actions() {
        let mut telemetry = Telemetry::default();
        telemetry.apply_nvme(crate::model::NvmeHealth {
            critical_warning: 0x1f,
            percentage_used: Some(12),
            ..Default::default()
        });
        let state = Dashboard {
            disks: vec![
                DiskView {
                    identity: identity("a", 0),
                    telemetry: Some(telemetry),
                    querying: false,
                    computing: false,
                    pending_telemetry: None,
                    error_message: None,
                },
                DiskView {
                    identity: identity("b", 1),
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
        let mut global = test_global_data(state, 80, 14);
        let layout = Layout::new(80, 14, global.state.disks.len(), 0);
        let click =
            |app: &mut DashboardApp, global: &mut GlobalData<Dashboard, AppSignal>, action| {
                let button = layout
                    .controls
                    .iter()
                    .find(|button| button.action == action)
                    .unwrap();
                assert_eq!(
                    layout.control_at(button.area.x, button.area.y),
                    Some(action)
                );
                dispatch_input(
                    app,
                    global,
                    mouse_event(
                        MouseInputKind::MouseDown(Button::Left),
                        button.area.x,
                        button.area.y,
                    ),
                )
            };

        click(&mut app, &mut global, FooterAction::NextDisk);
        assert_eq!(global.state.selected, 1);
        dispatch_input(
            &mut app,
            &mut global,
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Left),
            }),
        );
        assert_eq!(global.state.selected, 0);

        click(&mut app, &mut global, FooterAction::ScrollDown);
        assert_eq!(global.state.content_scroll, 1);
        dispatch_input(
            &mut app,
            &mut global,
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Up),
            }),
        );
        assert_eq!(global.state.content_scroll, 0);
        dispatch_input(
            &mut app,
            &mut global,
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Down),
            }),
        );
        assert_eq!(global.state.content_scroll, 1);
        click(&mut app, &mut global, FooterAction::ScrollUp);
        assert_eq!(global.state.content_scroll, 0);
    }

    #[test]
    fn compact_mouse_controls_cover_disk_selection_detail_scroll_and_exit() {
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
        let mut global = test_global_data(state, 30, 10);
        let layout = Layout::new(30, 10, 4, 0);
        assert!(layout.controls.is_empty());

        dispatch_input(
            &mut app,
            &mut global,
            mouse_event(
                MouseInputKind::ScrollDown,
                layout.tabs[0].x + 1,
                layout.tabs[0].y + 1,
            ),
        );
        assert_eq!(global.state.selected, 1);
        assert_eq!(global.state.scroll, 1);

        let selected_tab = Layout::new(30, 10, 4, global.state.scroll).tabs[0];
        dispatch_input(
            &mut app,
            &mut global,
            mouse_event(
                MouseInputKind::MouseDown(Button::Left),
                selected_tab.x + selected_tab.width / 2,
                selected_tab.y + selected_tab.height / 2,
            ),
        );
        assert_eq!(global.state.selected, 1);

        dispatch_input(
            &mut app,
            &mut global,
            mouse_event(MouseInputKind::ScrollDown, 15, layout.body_y),
        );
        assert_eq!(global.state.content_scroll, 1);

        assert!(matches!(
            dispatch_input(
                &mut app,
                &mut global,
                mouse_event(
                    MouseInputKind::MouseDown(Button::Left),
                    layout.quit.x + 1,
                    layout.quit.y + 1,
                ),
            ),
            EventPropagation::ExitMainEventLoop
        ));
    }

    #[test]
    fn detail_scroll_clamps_to_last_row_for_keyboard_mouse_and_viewport_changes() {
        let mut telemetry = Telemetry::default();
        telemetry.apply_nvme(crate::model::NvmeHealth {
            critical_warning: 0x1f,
            percentage_used: Some(12),
            ..Default::default()
        });
        let disk = DiskView {
            identity: identity("scrollable", 0),
            telemetry: Some(telemetry),
            querying: false,
            computing: false,
            pending_telemetry: None,
            error_message: None,
        };

        for (width, height) in [(80, 14), (30, 10)] {
            let mut app = DashboardApp {
                started: true,
                ..Default::default()
            };
            let mut global = test_global_data(
                Dashboard {
                    disks: vec![disk.clone()],
                    ..Default::default()
                },
                width,
                height,
            );
            let layout = Layout::new(width, height, 1, 0);
            let body_height = body_view_height(&layout, height);
            let expected_max =
                disk_content_line_count(&disk, width, body_height).saturating_sub(body_height);

            for _ in 0..20 {
                dispatch_input(
                    &mut app,
                    &mut global,
                    InputEvent::Keyboard(KeyPress::Plain {
                        key: Key::SpecialKey(SpecialKey::Down),
                    }),
                );
            }
            assert_eq!(
                global.state.content_scroll, expected_max,
                "{width}x{height}"
            );
            dispatch_input(
                &mut app,
                &mut global,
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::PageDown),
                }),
            );
            assert_eq!(global.state.content_scroll, expected_max);
            dispatch_input(
                &mut app,
                &mut global,
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Up),
                }),
            );
            assert_eq!(global.state.content_scroll, expected_max - 1);

            for _ in 0..20 {
                dispatch_input(
                    &mut app,
                    &mut global,
                    mouse_event(MouseInputKind::ScrollDown, 0, layout.body_y),
                );
            }
            assert_eq!(global.state.content_scroll, expected_max);
            dispatch_input(
                &mut app,
                &mut global,
                mouse_event(MouseInputKind::ScrollDown, 0, layout.body_y),
            );
            assert_eq!(global.state.content_scroll, expected_max);
            dispatch_input(
                &mut app,
                &mut global,
                mouse_event(MouseInputKind::ScrollUp, 0, layout.body_y),
            );
            assert_eq!(global.state.content_scroll, expected_max - 1);

            for _ in 0..5 {
                dispatch_input(
                    &mut app,
                    &mut global,
                    mouse_event(MouseInputKind::ScrollDown, 0, layout.body_y),
                );
            }
            assert_eq!(global.state.content_scroll, expected_max);
            let rendered = rendered_text(&mut app, &mut global);
            assert!(rendered.contains("Backup da memória volátil"), "{rendered}");
            assert!(rendered.contains("Sair"), "{rendered}");
            assert!(
                rendered.contains(if width >= 54 {
                    "↑ Detalhes"
                } else {
                    "↑ Info"
                }),
                "{rendered}"
            );
            let last_visible_row = rendered.lines().nth(height - 2).unwrap().trim();
            assert!(last_visible_row.contains("LIGADO"), "{rendered}");

            if width == 80 {
                global.window_size = r3bl_tui::Size::new((
                    r3bl_tui::ColWidth::new(width),
                    r3bl_tui::RowHeight::new(10),
                ));
                assert!(global.state.content_scroll > 0);
                let resized = rendered_text(&mut app, &mut global);
                let resized_layout = Layout::new(width, 10, 1, global.state.scroll);
                let resized_body_height = body_view_height(&resized_layout, 10);
                let resized_max = disk_content_line_count(&disk, width, resized_body_height)
                    .saturating_sub(resized_body_height);
                assert_eq!(global.state.content_scroll, resized_max);
                assert!(resized.contains("LIGADO"), "{resized}");
                global.state.disks.clear();
                let _ = rendered_text(&mut app, &mut global);
                assert_eq!(global.state.content_scroll, 0);

                global.state.disks = vec![
                    disk.clone(),
                    DiskView {
                        identity: identity("other", 1),
                        telemetry: None,
                        querying: false,
                        computing: false,
                        pending_telemetry: None,
                        error_message: None,
                    },
                ];
                global.state.selected = 0;
                global.state.content_scroll = expected_max;
                dispatch_input(
                    &mut app,
                    &mut global,
                    InputEvent::Keyboard(KeyPress::Plain {
                        key: Key::SpecialKey(SpecialKey::Tab),
                    }),
                );
                assert_eq!(global.state.selected, 1);
                assert_eq!(global.state.content_scroll, 0);
            }
        }
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
            assert!(rendered.contains("ETA"), "{width}x{height}: {rendered}");
        }

        let unknown_type = DiskView {
            identity: DiskIdentity {
                kind: DiskKind::Unknown,
                ..identity("unknown-type", 0)
            },
            telemetry: Some(Telemetry::default()),
            querying: false,
            computing: false,
            pending_telemetry: None,
            error_message: None,
        };
        let mut app = DashboardApp {
            started: true,
            ..Default::default()
        };
        let mut global = test_global_data(
            Dashboard {
                disks: vec![unknown_type],
                ..Default::default()
            },
            30,
            10,
        );
        let rendered = rendered_text(&mut app, &mut global);
        assert!(
            rendered.contains("Sem ETA: tipo desconhecido"),
            "{rendered}"
        );
        assert!(
            rendered
                .lines()
                .any(|line| { line.contains("TIPO") && line.contains("Desconhecido") }),
            "{rendered}"
        );
        assert!(!rendered.contains("Tipo desconheci"), "{rendered}");

        global.state.disks[0].telemetry = None;
        let rendered = rendered_text(&mut app, &mut global);
        assert!(
            rendered
                .lines()
                .any(|line| { line.contains("TIPO") && line.contains("Desconhecido") }),
            "{rendered}"
        );
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
        let mut primary_identity = identity("test", 4);
        primary_identity.model = Some("MODELO-UNICO".into());
        let disk = DiskView {
            identity: primary_identity,
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
        for (width, height) in [(100, 28), (70, 20), (30, 10), (12, 4), (1, 1), (1, 8)] {
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
                let layout = Layout::new(width, height, state.disks.len(), state.scroll);
                assert!(
                    matches!(
                        buffer[layout.tabs[0].y][layout.tabs[0].x],
                        r3bl_tui::PixelChar::PlainText { style, .. }
                            if style.color_bg == Some(TuiColor::Rgb(TOKYO_BG_VISUAL))
                    ),
                    "bg={:?} wanted={:?}",
                    match buffer[layout.tabs[0].y][layout.tabs[0].x] {
                        r3bl_tui::PixelChar::PlainText { style, .. } => style.color_bg,
                        _ => None,
                    },
                    TuiColor::Rgb(TOKYO_BG_VISUAL)
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
                assert!(cell_has_colors(&buffer[1][1], TOKYO_BLUE, TOKYO_BG_DARK));
                let layout = Layout::new(width, height, state.disks.len(), state.scroll);
                assert!(cell_has_colors(
                    &buffer[layout.tabs[0].y][layout.tabs[0].x],
                    TOKYO_FG,
                    TOKYO_BG_VISUAL
                ));
                for (position, tab) in layout.tabs.iter().enumerate() {
                    let (foreground, background) =
                        if layout.tab_indices[position] == global.state.selected {
                            (TOKYO_FG, TOKYO_BG_VISUAL)
                        } else {
                            (TOKYO_FG_DARK, TOKYO_BG_DARK1)
                        };
                    for y in tab.y..tab.y + tab.height {
                        for x in tab.x..tab.x + tab.width {
                            assert!(cell_has_colors(&buffer[y][x], foreground, background));
                        }
                    }
                }
                let tab_row = rendered.lines().nth(layout.tabs[0].y + 1).unwrap();
                assert!(tab_row.contains("> Disco #4"), "{tab_row}");
                assert!(!tab_row.contains("MODELO-UNICO"), "{tab_row}");
                assert!(
                    rendered
                        .lines()
                        .nth(layout.body_y)
                        .unwrap()
                        .contains("MODELO-UNICO"),
                    "{rendered}"
                );
                let health_row = layout.body_y + 1;
                assert!(cell_has_colors(
                    &buffer[health_row][1],
                    TOKYO_GREEN,
                    TOKYO_BG_DARK1
                ));
                let eta_row = rendered
                    .lines()
                    .position(|line| line.contains("ETA"))
                    .unwrap();
                assert!(cell_has_colors(
                    &buffer[eta_row][1],
                    TOKYO_FG_DARK,
                    TOKYO_BG_DARK
                ));
                for y in layout.quit.y..layout.quit.y + layout.quit.height {
                    for x in layout.quit.x..layout.quit.x + layout.quit.width {
                        assert!(cell_has_colors(&buffer[y][x], TOKYO_BG_DARK1, TOKYO_RED));
                    }
                }
                for control in &layout.controls {
                    let color = match control.action {
                        FooterAction::PreviousDisk | FooterAction::NextDisk => TOKYO_BLUE,
                        FooterAction::ScrollUp | FooterAction::ScrollDown => TOKYO_CYAN,
                    };
                    for y in control.area.y..control.area.y + control.area.height {
                        for x in control.area.x..control.area.x + control.area.width {
                            assert!(cell_has_colors(&buffer[y][x], TOKYO_BG_DARK1, color));
                        }
                    }
                }
                assert!(
                    rendered.contains("#4") && rendered.contains("#8") && rendered.contains("#12"),
                    "{rendered}"
                );
                assert!(rendered.contains("SAÚDE · SAUDÁVEL"));
                assert!(
                    ascii_score_lines("88%")
                        .iter()
                        .all(|score_row| rendered.contains(score_row.trim_end()))
                );
            }
            if width == 30 {
                let layout = Layout::new(width, height, state.disks.len(), state.scroll);
                assert!(layout.controls.is_empty());
                for y in layout.quit.y..layout.quit.y + layout.quit.height {
                    for x in layout.quit.x..layout.quit.x + layout.quit.width {
                        assert!(cell_has_colors(&buffer[y][x], TOKYO_BG_DARK1, TOKYO_RED));
                    }
                }
                for y in layout.tabs[0].y..layout.tabs[0].y + layout.tabs[0].height {
                    for x in layout.tabs[0].x..layout.tabs[0].x + layout.tabs[0].width {
                        assert!(cell_has_colors(&buffer[y][x], TOKYO_FG, TOKYO_BG_VISUAL));
                    }
                }
            }
            if width == 12 && height >= 8 {
                assert!(rendered.contains("88%"), "{rendered}");
                assert!(rendered.contains("ETA"), "{rendered}");
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
    fn coarse_smart_pass_explains_low_confidence_in_full_and_compact_views() {
        let disk = DiskView {
            identity: DiskIdentity {
                kind: DiskKind::Hdd,
                ..identity("coarse-smart", 4)
            },
            telemetry: Some(Telemetry {
                overall_failure: QueryState::Success(false),
                ..Default::default()
            }),
            querying: false,
            computing: false,
            pending_telemetry: None,
            error_message: None,
        };
        let state = Dashboard {
            disks: vec![disk],
            ..Default::default()
        };
        let mut app = DashboardApp {
            started: true,
            ..Default::default()
        };
        for (width, height, health_label, confidence_note) in [
            (
                100,
                28,
                "SAÚDE · LEITURA SMART BÁSICA",
                "SMART básico: escore de baixa confiança; sem mapeamento detalhado",
            ),
            (
                70,
                20,
                "SAÚDE · LEITURA SMART BÁSICA",
                "SMART básico: escore de baixa confiança; sem mapeamento detalhado",
            ),
            (
                30,
                10,
                "SAÚDE · SMART BÁSICO · 100%",
                "SMART básico: baixa confiança",
            ),
        ] {
            let mut global = test_global_data(state.clone(), width, height);
            let rendered = rendered_text(&mut app, &mut global);
            assert!(
                rendered.contains(health_label),
                "{width}x{height}: {rendered}"
            );
            assert!(
                rendered.contains(confidence_note),
                "{width}x{height}: {rendered}"
            );
            assert!(
                rendered.contains("ETA não aplicável"),
                "{width}x{height}: {rendered}"
            );
            assert!(
                !rendered.contains("SAUDÁVEL"),
                "{width}x{height}: {rendered}"
            );
        }
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
