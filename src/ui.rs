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
    collections::{HashMap, HashSet, VecDeque},
    fmt::{Display, Formatter},
    io::Write,
    path::PathBuf,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::mpsc::{Sender, UnboundedReceiver, UnboundedSender},
    task::JoinSet,
    time::{Duration, MissedTickBehavior},
};

mod presentation;
use presentation::{disk_detail_lines, render_disk_summary, render_panel};

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
#[cfg(test)]
const TOKYO_ORANGE: RgbValue = RgbValue {
    red: 255,
    green: 150,
    blue: 108,
};
#[cfg(test)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NotificationSeverity {
    Info,
    Warning,
    Critical,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Notification {
    generation: u64,
    owner: Option<String>,
    disk_id: Option<String>,
    title: String,
    details: Vec<String>,
    severity: NotificationSeverity,
    expires_at: Instant,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dashboard {
    pub disks: Vec<DiskView>,
    pub selected: usize,
    pub scroll: usize,
    pub hovered_tab: Option<usize>,
    pub content_scroll: usize,
    pub notice: Option<String>,
    notifications: VecDeque<Notification>,
    notifications_open: bool,
    details_open: bool,
    notification_scroll: usize,
    last_issue_keys: HashMap<String, HashSet<String>>,
    inventory_initialized: bool,
    toast_was_active: bool,
    pub history: History,
    pub generation: u64,
    collection_generation: u64,
    pub frame: usize,
}

const MAX_NOTIFICATION_HISTORY: usize = 64;
const TOAST_DURATION: Duration = Duration::from_secs(6);

fn active_notification(state: &Dashboard, now: Instant) -> Option<&Notification> {
    state
        .notifications
        .iter()
        .rev()
        .find(|notice| notice.generation == state.collection_generation && notice.expires_at > now)
}

fn push_notification(
    state: &mut Dashboard,
    disk_id: Option<String>,
    title: String,
    details: Vec<String>,
    severity: NotificationSeverity,
    now: Instant,
) {
    state.notifications.push_back(Notification {
        generation: state.collection_generation,
        owner: None,
        disk_id,
        title,
        details,
        severity,
        expires_at: now + TOAST_DURATION,
    });
    while state.notifications.len() > MAX_NOTIFICATION_HISTORY {
        state.notifications.pop_front();
    }
    state.notification_scroll = 0;
}

fn reset_notifications_for_generation(state: &mut Dashboard) {
    state.notifications.clear();
    state.notification_scroll = 0;
    state.toast_was_active = false;
}

/// Notification collection epochs are independent of topology epochs: a slow
/// query for an unchanged device must not be starved by the 15-second poll.
fn begin_notification_collection(state: &mut Dashboard) {
    state.collection_generation = state.collection_generation.saturating_add(1);
    reset_notifications_for_generation(state);
}

#[allow(clippy::too_many_arguments)]
fn replace_current_notice(
    state: &mut Dashboard,
    owner: &str,
    disk_id: Option<String>,
    title: String,
    details: Vec<String>,
    severity: NotificationSeverity,
    changed: bool,
) {
    let previous_expiry = state
        .notifications
        .iter()
        .find(|n| n.owner.as_deref() == Some(owner))
        .map(|n| n.expires_at);
    state
        .notifications
        .retain(|n| n.owner.as_deref() != Some(owner));
    let now = Instant::now();
    push_notification(state, disk_id, title, details, severity, now);
    if let Some(notice) = state.notifications.back_mut() {
        notice.owner = Some(owner.to_owned());
        if !changed {
            notice.expires_at = previous_expiry.unwrap_or(now);
        }
    }
}

fn disk_label(identity: &DiskIdentity) -> String {
    let model = identity
        .model
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    match model {
        Some(model) => format!("Disco #{} · {model}", identity.index),
        None => format!("Disco #{}", identity.index),
    }
}

fn record_issue_transition(
    state: &mut Dashboard,
    owner: &str,
    current_keys: HashSet<String>,
) -> bool {
    let previous = state.last_issue_keys.entry(owner.to_owned()).or_default();
    let has_new_issue = current_keys.iter().any(|key| !previous.contains(key));
    *previous = current_keys;
    has_new_issue
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
                Key::Character('q') => {
                    return Ok(EventPropagation::ExitMainEventLoop);
                }
                Key::SpecialKey(SpecialKey::Esc)
                    if state.notifications_open || state.details_open =>
                {
                    state.notifications_open = false;
                    state.details_open = false;
                    return Ok(EventPropagation::ConsumedRender);
                }
                Key::SpecialKey(SpecialKey::Esc) => {
                    return Ok(EventPropagation::ExitMainEventLoop);
                }
                Key::Character('n') => {
                    toggle_notification_center(state);
                }
                Key::Character('d') => {
                    state.details_open = !state.details_open;
                    state.notifications_open = false;
                    state.content_scroll = 0;
                }
                Key::SpecialKey(SpecialKey::Left) | Key::SpecialKey(SpecialKey::BackTab) => {
                    apply_footer_action(state, FooterAction::PreviousDisk);
                }
                Key::SpecialKey(SpecialKey::Right) | Key::SpecialKey(SpecialKey::Tab) => {
                    apply_footer_action(state, FooterAction::NextDisk);
                }
                Key::SpecialKey(SpecialKey::Up) | Key::Character('k') => {
                    if state.notifications_open {
                        state.notification_scroll = state.notification_scroll.saturating_sub(1);
                    } else {
                        apply_footer_action(state, FooterAction::ScrollUp);
                    }
                }
                Key::SpecialKey(SpecialKey::Down) | Key::Character('j') => {
                    if state.notifications_open {
                        state.notification_scroll = state.notification_scroll.saturating_add(1);
                    } else {
                        apply_footer_action(state, FooterAction::ScrollDown);
                    }
                }
                Key::SpecialKey(SpecialKey::PageUp) => {
                    if state.notifications_open {
                        state.notification_scroll = state.notification_scroll.saturating_sub(5);
                    } else {
                        state.content_scroll = state.content_scroll.saturating_sub(5);
                    }
                }
                Key::SpecialKey(SpecialKey::PageDown) => {
                    if state.notifications_open {
                        state.notification_scroll = state.notification_scroll.saturating_add(5);
                    } else {
                        state.content_scroll = state.content_scroll.saturating_add(5);
                    }
                }
                _ => return Ok(EventPropagation::Propagate),
            },
            InputEvent::Mouse(mouse) => {
                let x: usize = (*mouse.pos.col_index).into();
                let y: usize = (*mouse.pos.row_index).into();
                let layout = layout_for_state(width, height, state);
                // The opaque overlay owns every input in its rectangle,
                // including blank cells above underlying footer/tab controls.
                if layout.panel.contains(x, y) {
                    match mouse.kind {
                        MouseInputKind::MouseDown(Button::Left) => {
                            if let Some(action) = layout.control_at(x, y) {
                                apply_footer_action(state, action);
                            }
                        }
                        MouseInputKind::ScrollUp => {
                            apply_footer_action(state, FooterAction::ScrollUp)
                        }
                        MouseInputKind::ScrollDown => {
                            apply_footer_action(state, FooterAction::ScrollDown)
                        }
                        MouseInputKind::MouseMove => state.hovered_tab = None,
                        _ => {}
                    }
                    clamp_content_scroll(state, width, height);
                    clamp_notification_scroll(state, width, height);
                    return Ok(EventPropagation::ConsumedRender);
                }
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
                        if layout.detail_button.contains(x, y) {
                            state.details_open = !state.details_open;
                            state.notifications_open = false;
                            state.content_scroll = 0;
                        } else if layout.notification_button.contains(x, y) {
                            toggle_notification_center(state);
                        } else if !state.notifications_open
                            && layout.toast.contains(x, y)
                            && active_notification(state, Instant::now()).is_some()
                        {
                            state.notifications_open = true;
                            state.details_open = false;
                            state.notification_scroll = 0;
                        } else if let Some(action) = layout.control_at(x, y) {
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
                    MouseInputKind::ScrollUp | MouseInputKind::ScrollDown => {}
                    _ => return Ok(EventPropagation::Propagate),
                }
            }
            _ => return Ok(EventPropagation::Propagate),
        }
        ensure_selected_visible(state, width, height);
        clamp_content_scroll(state, width, height);
        clamp_notification_scroll(state, width, height);
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
                let topology_changed =
                    state.inventory_initialized && *generation != state.generation;
                begin_notification_collection(state);
                state.generation = *generation;
                let old_id = state
                    .disks
                    .get(state.selected)
                    .map(|disk| disk.identity.id.clone());
                let previous_selection = state.selected;
                let previous = std::mem::take(&mut state.disks);
                record_issue_transition(state, "@inventory", HashSet::new());
                if topology_changed {
                    let previous_ids = previous
                        .iter()
                        .map(|disk| disk.identity.id.as_str())
                        .collect::<HashSet<_>>();
                    let next_ids = disks
                        .iter()
                        .map(|disk| disk.id.as_str())
                        .collect::<HashSet<_>>();
                    let mut changes = disks
                        .iter()
                        .filter(|disk| !previous_ids.contains(disk.id.as_str()))
                        .map(|disk| format!("Conectado: {}", disk_label(disk)))
                        .collect::<Vec<_>>();
                    changes.extend(
                        previous
                            .iter()
                            .filter(|disk| !next_ids.contains(disk.identity.id.as_str()))
                            .map(|disk| format!("Removido: {}", disk_label(&disk.identity))),
                    );
                    if !changes.is_empty() {
                        push_notification(
                            state,
                            None,
                            "Inventário de discos atualizado".into(),
                            changes,
                            NotificationSeverity::Info,
                            Instant::now(),
                        );
                    }
                }
                state.last_issue_keys.retain(|owner, _| {
                    owner == "@inventory"
                        || disks
                            .iter()
                            .any(|d| owner == &d.id || owner == &format!("query:{}", d.id))
                });
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
                state.inventory_initialized = true;
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
                begin_notification_collection(state);
                state.notice = Some("Não foi possível localizar os discos".into());
                let changed = record_issue_transition(
                    state,
                    "@inventory",
                    HashSet::from(["enumeration-failed".to_owned()]),
                );
                replace_current_notice(
                    state,
                    "@inventory",
                    None,
                    "Não foi possível atualizar o inventário".into(),
                    vec!["A lista atual de discos foi mantida.".into()],
                    NotificationSeverity::Warning,
                    changed,
                );
            }
            AppSignal::QueryFinished {
                generation,
                disk_id,
                result,
            } => {
                if *generation != state.generation {
                    return Ok(EventPropagation::Consumed);
                }
                let mut disk_name = None;
                let mut query_error = None;
                if let Some(disk) = state
                    .disks
                    .iter_mut()
                    .find(|disk| disk.identity.id == *disk_id)
                {
                    disk_name = Some(disk_label(&disk.identity));
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
                        if let Err(error) = result {
                            let message = match error {
                                QueryErrorState::Permission => {
                                    "Permissão insuficiente para consultar o dispositivo"
                                }
                                QueryErrorState::Unsupported => {
                                    "Leitura de saúde não compatível com o dispositivo"
                                }
                                QueryErrorState::Transient | QueryErrorState::Partial => {
                                    "Não foi possível consultar os dados de saúde"
                                }
                            };
                            disk.error_message = Some(message.into());
                            query_error = Some((format!("{error:?}"), message.to_owned()));
                        }
                    }
                }
                if let Some(disk_name) = disk_name {
                    let issue_owner = format!("query:{disk_id}");
                    if let Some((key, message)) = query_error {
                        let changed =
                            record_issue_transition(state, &issue_owner, HashSet::from([key]));
                        // The current query supersedes prior health/error text
                        // for this disk, including a recovery within this poll.
                        state.notifications.retain(|n| {
                            n.disk_id.as_deref() != Some(disk_id)
                                || n.owner.as_deref() == Some(issue_owner.as_str())
                        });
                        replace_current_notice(
                            state,
                            &issue_owner,
                            Some(disk_id.clone()),
                            format!("Falha na consulta · {disk_name}"),
                            vec![message],
                            NotificationSeverity::Warning,
                            changed,
                        );
                    } else {
                        record_issue_transition(state, &issue_owner, HashSet::new());
                        state
                            .notifications
                            .retain(|n| n.owner.as_deref() != Some(issue_owner.as_str()));
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
                let mut health_notice = None;
                if let Some(disk) = state
                    .disks
                    .iter_mut()
                    .find(|disk| disk.identity.id == *disk_id)
                {
                    disk.computing = false;
                    if let Some(telemetry) = disk.pending_telemetry.take() {
                        let kind = effective_disk_kind(&disk.identity, &telemetry);
                        let score = score_health(kind, &telemetry);
                        let (issue_keys, details, severity) =
                            health_notification_snapshot(kind, &score, &telemetry);
                        health_notice = Some((
                            disk.identity.id.clone(),
                            disk_label(&disk.identity),
                            issue_keys,
                            details,
                            severity,
                        ));
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
                if let Some((disk_id, label, issue_keys, details, severity)) = health_notice {
                    let has_issues = !issue_keys.is_empty();
                    let changed = record_issue_transition(state, &disk_id, issue_keys);
                    state.notifications.retain(|n| {
                        n.disk_id.as_deref() != Some(disk_id.as_str())
                            || (has_issues && n.owner.as_deref() == Some(disk_id.as_str()))
                    });
                    if !has_issues {
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    let title = match severity {
                        NotificationSeverity::Info => "Informação de saúde",
                        NotificationSeverity::Warning => "Atenção à saúde",
                        NotificationSeverity::Critical => "Falha de saúde",
                    };
                    replace_current_notice(
                        state,
                        &disk_id,
                        Some(disk_id.clone()),
                        format!("{title} · {label}"),
                        details,
                        severity,
                        changed,
                    );
                }
            }
            AppSignal::HistoryLoaded(history) => state.history = history.clone(),
            AppSignal::Frame => {
                let active_toast = active_notification(state, Instant::now()).is_some();
                let toast_expired = state.toast_was_active && !active_toast;
                state.toast_was_active = active_toast;
                if !state
                    .disks
                    .iter()
                    .any(|disk| disk.querying || disk.computing)
                    && !active_toast
                    && !toast_expired
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
        clamp_notification_scroll(&mut global.state, width, height);
        let state = &global.state;
        let layout = layout_for_state(width, height, state);
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
        let title_right = if layout.detail_button.visible() {
            layout.detail_button.x
        } else if layout.notification_button.visible() {
            layout.notification_button.x
        } else {
            layout.quit.x
        };
        let title_width = title_right.saturating_sub(title_x + 1);
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
        if layout.notification_button.visible() {
            let label = if layout.notification_button.width >= 11 {
                if state.notifications_open {
                    "Fechar [N]".to_owned()
                } else {
                    format!("Avisos {} [N]", state.notifications.len().min(99))
                }
            } else {
                if state.notifications_open { "×" } else { "!" }.to_owned()
            };
            let active = active_notification(state, Instant::now());
            let (fg, bg) = match active.map(|notice| notice.severity) {
                Some(NotificationSeverity::Critical) => (TOKYO_BG_DARK1, TOKYO_RED),
                Some(NotificationSeverity::Warning) => (TOKYO_BG_DARK1, TOKYO_YELLOW),
                Some(NotificationSeverity::Info) => (TOKYO_BG_DARK1, TOKYO_BLUE),
                None if !state.notifications.is_empty() => (TOKYO_FG, TOKYO_BG_VISUAL),
                None => (TOKYO_FG_DARK, TOKYO_BG_DARK1),
            };
            paint_button(&mut ops, layout.notification_button, &label, fg, bg, true);
        }
        if layout.detail_button.visible() {
            let label = if state.details_open {
                if layout.detail_button.width >= 8 {
                    "Fechar [D]"
                } else {
                    "×"
                }
            } else if layout.detail_button.width >= 10 {
                "Detalhes [D]"
            } else {
                "D"
            };
            paint_button(
                &mut ops,
                layout.detail_button,
                label,
                TOKYO_FG,
                TOKYO_BG_DARK1,
                true,
            );
        }
        for (position, area) in layout.tabs.iter().enumerate() {
            let index = layout.tab_indices[position];
            let disk = &state.disks[index];
            let label = tab_label(&disk.identity);
            let (fg, bg) = if index == state.selected {
                (TOKYO_FG, TOKYO_BG_VISUAL)
            } else if state.hovered_tab == Some(index) {
                (TOKYO_FG, TOKYO_BG_HIGHLIGHT)
            } else {
                (TOKYO_FG_DARK, TOKYO_BG_DARK1)
            };
            paint_button(&mut ops, *area, &label, fg, bg, true);
        }
        let body_height = body_view_height(&layout, height);
        if height > layout.body_y && body_height > 0 {
            if let Some(disk) = state.disks.get(state.selected) {
                let main_width = if layout.panel.visible() && layout.panel.x > 0 {
                    layout.panel.x
                } else {
                    width
                };
                render_disk_summary(
                    &mut ops,
                    disk,
                    &state.history,
                    HitArea {
                        x: 0,
                        y: layout.body_y,
                        width: main_width,
                        height: body_height,
                    },
                );
            } else {
                paint_background(
                    &mut ops,
                    1.min(width.saturating_sub(1)),
                    layout.body_y,
                    state.notice.as_deref().unwrap_or("Procurando discos…"),
                    TOKYO_FG_DARK,
                    false,
                    width.saturating_sub(2),
                    Some(TOKYO_BG),
                );
            }
        }
        if layout.toast.visible() && layout.toast_reserved {
            fill_row(&mut ops, layout.toast.y, width, TOKYO_BG_DARK1);
            if !state.notifications_open
                && let Some(notice) = active_notification(state, Instant::now())
            {
                let color = match notice.severity {
                    NotificationSeverity::Info => TOKYO_CYAN,
                    NotificationSeverity::Warning => TOKYO_YELLOW,
                    NotificationSeverity::Critical => TOKYO_RED,
                };
                let toast_text = "Novo aviso · N ou clique para abrir";
                paint_background(
                    &mut ops,
                    layout.toast.x,
                    layout.toast.y,
                    toast_text,
                    color,
                    true,
                    layout.toast.width,
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
        render_panel(&mut ops, state, &layout);
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
        FooterAction::ScrollUp if state.notifications_open => {
            state.notification_scroll = state.notification_scroll.saturating_sub(1);
        }
        FooterAction::ScrollDown if state.notifications_open => {
            state.notification_scroll = state.notification_scroll.saturating_add(1);
        }
        FooterAction::ScrollUp if state.details_open => {
            state.content_scroll = state.content_scroll.saturating_sub(1)
        }
        FooterAction::ScrollDown if state.details_open => {
            state.content_scroll = state.content_scroll.saturating_add(1)
        }
        FooterAction::ScrollUp | FooterAction::ScrollDown => {}
    }
}

fn toggle_notification_center(state: &mut Dashboard) {
    state.notifications_open = !state.notifications_open;
    state.details_open = false;
    if state.notifications_open {
        state.notification_scroll = 0;
    }
}

fn tab_label(identity: &DiskIdentity) -> String {
    match identity
        .model
        .as_deref()
        .filter(|model| !model.trim().is_empty())
    {
        Some(model) => format!("#{} {model}", identity.index),
        None => format!("Disco #{}", identity.index),
    }
}

fn ensure_selected_visible(state: &mut Dashboard, width: usize, height: usize) {
    let layout = Layout::for_state(width, height, state);
    if !layout.tab_indices.contains(&state.selected) {
        state.scroll = state.selected.min(state.disks.len().saturating_sub(1));
    }
}

fn clamp_tab_scroll(state: &mut Dashboard, _width: usize, _height: usize) {
    state.scroll = state.scroll.min(state.disks.len().saturating_sub(1));
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
    pub notification_button: HitArea,
    pub detail_button: HitArea,
    pub panel: HitArea,
    pub toast: HitArea,
    toast_reserved: bool,
    controls: Vec<FooterControl>,
    pub body_y: usize,
}

fn tab_button_width(index_digits: usize) -> usize {
    // Two fixed selector cells, "Disco #", the longest index, and one cell
    // of horizontal padding on either side.
    2 + 7 + index_digits + 2
}

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
    #[cfg(test)]
    pub fn new(width: usize, height: usize, disks: usize, scroll: usize) -> Self {
        let index_digits = disks.saturating_sub(1).max(1).to_string().len();
        Self::new_with_index_digits(width, height, disks, scroll, index_digits, false)
    }

    fn for_state(width: usize, height: usize, state: &Dashboard) -> Self {
        let mut layout = Self::new_with_index_digits(
            width,
            height,
            state.disks.len(),
            state.scroll,
            1,
            state.notifications_open,
        );
        if width == 0 || height == 0 {
            return layout;
        }
        let button_height = layout.quit.height;
        let toggles_wide = width >= 50;
        let toggle_width = if toggles_wide {
            14
        } else if width >= 12 {
            3
        } else {
            1
        };
        layout.notification_button = if !state.details_open && width >= 4 {
            HitArea {
                x: layout.quit.x.saturating_sub(toggle_width + 1),
                y: 0,
                width: toggle_width.min(layout.quit.x),
                height: button_height,
            }
        } else {
            HitArea::default()
        };
        layout.detail_button = if !state.notifications_open && width >= 8 {
            let right = if layout.notification_button.visible() {
                layout.notification_button.x
            } else {
                layout.quit.x
            };
            HitArea {
                x: right.saturating_sub(toggle_width + 1),
                y: 0,
                width: toggle_width.min(right.saturating_sub(1)),
                height: button_height,
            }
        } else {
            HitArea::default()
        };
        // Names stay on permanent, horizontally navigable tabs. Each tab has
        // one cell of padding and clips graphemes without breaking identities.
        layout.tabs.clear();
        layout.tab_indices.clear();
        let margin = usize::from(width >= 24);
        let mut x = margin;
        if width >= 3 && height > 2 * button_height {
            for (index, disk) in state.disks.iter().enumerate().skip(state.scroll) {
                let remaining = width.saturating_sub(x + margin);
                let wanted = (display_width(&tab_label(&disk.identity)) + 2).min(34);
                if remaining < wanted && !layout.tabs.is_empty() {
                    break;
                }
                let tab_width = wanted.min(remaining);
                if tab_width == 0 {
                    break;
                }
                layout.tabs.push(HitArea {
                    x,
                    y: button_height,
                    width: tab_width,
                    height: button_height,
                });
                layout.tab_indices.push(index);
                x += tab_width + 1;
            }
        }
        layout.controls = footer_controls(width, height, false);
        layout.controls.retain(|c| {
            matches!(
                c.action,
                FooterAction::PreviousDisk | FooterAction::NextDisk
            )
        });
        center_controls(&mut layout.controls, width);
        if state.notifications_open || state.details_open {
            let panel_width = if width >= 72 {
                45.min(width / 2)
            } else {
                width
            };
            layout.panel = HitArea {
                x: width - panel_width,
                y: layout.body_y,
                width: panel_width,
                height: height.saturating_sub(layout.body_y),
            };
            // Keep both disk buttons reachable on the uncovered main side.
            layout.controls = footer_controls(layout.panel.x, height, false);
            layout.controls.retain(|c| {
                matches!(
                    c.action,
                    FooterAction::PreviousDisk | FooterAction::NextDisk
                )
            });
            center_controls(&mut layout.controls, layout.panel.x);
            if panel_width >= 13 && layout.panel.height >= 10 {
                for (offset, label, action) in [
                    (0, "↑", FooterAction::ScrollUp),
                    (6, "↓", FooterAction::ScrollDown),
                ] {
                    layout.controls.push(FooterControl {
                        area: HitArea {
                            x: layout.panel.x + (panel_width - 11) / 2 + offset,
                            y: height - 4,
                            width: 5,
                            height: 3,
                        },
                        label,
                        action,
                    });
                }
            }
        }
        layout
    }

    fn new_with_index_digits(
        width: usize,
        height: usize,
        disks: usize,
        scroll: usize,
        index_digits: usize,
        notifications_open: bool,
    ) -> Self {
        if width == 0 || height == 0 {
            return Self::default();
        }
        // Normal-sized buttons have one cell of vertical padding above and below
        // their centered label. Preserve a one-row escape hatch for tiny terminals.
        let button_height = if height >= 11 { 3 } else { 1 };
        let quit = if width >= 21 {
            HitArea {
                x: width - 7,
                y: 0,
                width: 6,
                height: button_height,
            }
        } else if width >= 18 {
            HitArea {
                x: width - 4,
                y: 0,
                width: 3,
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
        let notification_button = if width >= 36 {
            let button_width = 11;
            HitArea {
                x: quit.x.saturating_sub(button_width + 1),
                y: 0,
                width: button_width,
                height: button_height,
            }
        } else if width >= 10 {
            HitArea {
                x: quit.x.saturating_sub(2),
                y: 0,
                width: 1,
                height: button_height,
            }
        } else if width >= 2 {
            HitArea {
                x: width - 2,
                y: 0,
                width: 1,
                height: button_height,
            }
        } else {
            HitArea::default()
        };
        let mut tabs = Vec::new();
        let mut tab_indices = Vec::new();
        // Leave a quiet one-cell gutter around the horizontal tab strip when
        // the terminal can spare it; narrow terminals keep the full width.
        let side_margin = usize::from(width >= 24);
        let mut x = side_margin;
        let y = button_height;
        let start = scroll.min(disks.saturating_sub(1));
        let max_tab_width = tab_button_width(index_digits);
        if width >= 3 && height > 2 * button_height {
            for index in start..disks {
                let remaining = width.saturating_sub(x + side_margin);
                if remaining < max_tab_width && !tabs.is_empty() {
                    break;
                }
                let available = max_tab_width.min(remaining);
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
        let controls = footer_controls(width, height, notifications_open);
        let footer_height = controls.first().map_or(0, |control| control.area.height);
        let toast = if width >= 12 && height >= 12 {
            HitArea {
                x: 1.min(width.saturating_sub(1)),
                y: height.saturating_sub(footer_height + 1),
                width: width.saturating_sub(2),
                height: 1,
            }
        } else {
            HitArea::default()
        };
        Self {
            tabs,
            tab_indices,
            quit,
            notification_button,
            detail_button: HitArea::default(),
            panel: HitArea::default(),
            toast,
            toast_reserved: false,
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

fn center_controls(controls: &mut [FooterControl], width: usize) {
    if let (Some(first), Some(last)) = (controls.first(), controls.last()) {
        let old_x = first.area.x;
        let occupied = last.area.x + last.area.width - old_x;
        let centered = width.saturating_sub(occupied) / 2;
        for control in controls {
            control.area.x = control.area.x - old_x + centered;
        }
    }
}

fn footer_controls(width: usize, height: usize, notifications_open: bool) -> Vec<FooterControl> {
    // Compact screens reserve their height for the health summary. The same
    // mouse actions remain available through tab clicks and wheel scrolling.
    if width < 21 || height < 17 {
        return Vec::new();
    }
    let button_height = 3;
    let (scroll_up_label, scroll_down_label) = if notifications_open {
        if width >= 39 {
            ("↑ Avisos", "↓ Avisos")
        } else if width >= 33 {
            ("↑Avis", "↓Avis")
        } else {
            ("↑A", "↓A")
        }
    } else if width >= 54 {
        ("↑ Detalhes", "↓ Detalhes")
    } else if width >= 39 {
        ("↑ Info", "↓ Info")
    } else if width >= 33 {
        ("↑Dados", "↓Dados")
    } else {
        ("↑I", "↓I")
    };
    let controls = if width >= 54 {
        vec![
            ("← Discos", FooterAction::PreviousDisk),
            ("→ Discos", FooterAction::NextDisk),
            (scroll_up_label, FooterAction::ScrollUp),
            (scroll_down_label, FooterAction::ScrollDown),
        ]
    } else if width >= 39 {
        vec![
            ("← Disco", FooterAction::PreviousDisk),
            ("→ Disco", FooterAction::NextDisk),
            (scroll_up_label, FooterAction::ScrollUp),
            (scroll_down_label, FooterAction::ScrollDown),
        ]
    } else if width >= 33 {
        vec![
            ("←Dsc", FooterAction::PreviousDisk),
            ("→Dsc", FooterAction::NextDisk),
            (scroll_up_label, FooterAction::ScrollUp),
            (scroll_down_label, FooterAction::ScrollDown),
        ]
    } else {
        vec![
            ("←D", FooterAction::PreviousDisk),
            ("→D", FooterAction::NextDisk),
            (scroll_up_label, FooterAction::ScrollUp),
            (scroll_down_label, FooterAction::ScrollDown),
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
    height.saturating_sub(layout.body_y + footer_height + usize::from(layout.toast_reserved))
}

fn layout_for_state(width: usize, height: usize, state: &Dashboard) -> Layout {
    let mut layout = Layout::for_state(width, height, state);
    layout.toast_reserved = !state.notifications_open
        && !state.details_open
        && active_notification(state, Instant::now()).is_some()
        && layout.toast.visible();
    layout
}

fn panel_visible_lines(layout: &Layout) -> usize {
    let controls_height = usize::from(
        layout
            .controls
            .iter()
            .any(|c| layout.panel.contains(c.area.x, c.area.y)),
    ) * 4;
    layout.panel.height.saturating_sub(4 + controls_height)
}

fn clamp_content_scroll(state: &mut Dashboard, width: usize, height: usize) {
    if !state.details_open {
        state.content_scroll = 0;
        return;
    }
    let layout = layout_for_state(width, height, state);
    let count = state
        .disks
        .get(state.selected)
        .map(|d| {
            disk_detail_lines(
                d,
                &state.history,
                layout.panel.width.saturating_sub(2).max(1),
            )
            .len()
        })
        .unwrap_or_default();
    state.content_scroll = state
        .content_scroll
        .min(count.saturating_sub(panel_visible_lines(&layout)));
}

#[derive(Clone, Debug)]
struct NotificationLine {
    text: String,
    foreground: RgbValue,
    bold: bool,
}

fn notification_center_lines(state: &Dashboard, max_width: usize) -> Vec<NotificationLine> {
    let mut lines = Vec::new();
    for notice in state.notifications.iter().rev() {
        let color = match notice.severity {
            NotificationSeverity::Info => TOKYO_CYAN,
            NotificationSeverity::Warning => TOKYO_YELLOW,
            NotificationSeverity::Critical => TOKYO_RED,
        };
        lines.extend(wrap_text(&notice.title, max_width).into_iter().map(|text| {
            NotificationLine {
                text,
                foreground: color,
                bold: true,
            }
        }));
        for detail in &notice.details {
            let (prefix, indent_width) = if max_width >= 4 {
                ("  · ", 4)
            } else if max_width >= 2 {
                ("· ", 2)
            } else {
                ("", 0)
            };
            let indent = " ".repeat(indent_width);
            let wrapped = wrap_text(detail, max_width.saturating_sub(indent_width).max(1));
            lines.extend(
                wrapped
                    .into_iter()
                    .enumerate()
                    .map(|(index, text)| NotificationLine {
                        text: format!("{}{}", if index == 0 { prefix } else { &indent }, text),
                        foreground: TOKYO_FG,
                        bold: false,
                    }),
            );
        }
        lines.push(NotificationLine {
            text: String::new(),
            foreground: TOKYO_FG_DARK,
            bold: false,
        });
    }
    if lines.is_empty() {
        lines.extend(
            wrap_text("Nenhum aviso nesta coleta", max_width)
                .into_iter()
                .map(|text| NotificationLine {
                    text,
                    foreground: TOKYO_FG_DARK,
                    bold: false,
                }),
        );
    }
    lines
}

fn clamp_notification_scroll(state: &mut Dashboard, width: usize, height: usize) {
    if !state.notifications_open {
        return;
    }
    let layout = layout_for_state(width, height, state);
    state.notification_scroll = state.notification_scroll.min(
        notification_center_lines(state, layout.panel.width.saturating_sub(2).max(1))
            .len()
            .saturating_sub(panel_visible_lines(&layout)),
    );
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

/// A valid NVMe log or verified ATA SSD wear profile can fill a missing kind.
/// Preserve explicit discovery classifications; failed probes are not evidence.
fn effective_disk_kind(identity: &DiskIdentity, telemetry: &Telemetry) -> DiskKind {
    if identity.kind != DiskKind::Unknown {
        return identity.kind;
    }
    if matches!(telemetry.nvme, QueryState::Success(_)) {
        return DiskKind::Nvme;
    }
    if telemetry.endurance_percent.is_some()
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
            ("ATENÇÃO · SINAIS SMART", TOKYO_YELLOW)
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
                if crate::ata::unique_attribute(ata, attribute.id).is_some()
                    && (1..=254).contains(&attribute.current)
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
            HealthWarningReason::AtaReallocatedSectors => {
                vec!["Setores realocados registrados na mídia".into()]
            }
            HealthWarningReason::AtaSpinRetries => {
                vec!["Tentativas repetidas de giro registradas".into()]
            }
            HealthWarningReason::AtaReportedUncorrectable => {
                vec!["Erros incorrigíveis reportados pelo dispositivo".into()]
            }
            HealthWarningReason::AtaPendingSectors => {
                vec!["Setores pendentes aguardando remapeamento".into()]
            }
            HealthWarningReason::AtaOfflineUncorrectable => {
                vec!["Setores incorrigíveis encontrados em verificações offline".into()]
            }
            HealthWarningReason::AtaRawEncodingAmbiguous => {
                vec!["Codificação bruta SMART ambígua; índice de saúde HDD indisponível".into()]
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

fn health_notification_snapshot(
    kind: DiskKind,
    health: &crate::health::HealthScore,
    telemetry: &Telemetry,
) -> (HashSet<String>, Vec<String>, NotificationSeverity) {
    let basic_smart = is_basic_smart_pass(health);
    let telemetry_issue = relevant_telemetry_issue(kind, telemetry);
    let mut keys = health
        .warnings
        .iter()
        .map(|warning| format!("warning:{warning:?}"))
        .collect::<HashSet<_>>();
    if health.outcome == HealthOutcome::Fail {
        keys.insert("health-failure".into());
    }
    if health.outcome == HealthOutcome::Unknown {
        keys.insert("health-unavailable".into());
    }
    if health.score <= 50 && health.outcome == HealthOutcome::Pass {
        keys.insert("low-score".into());
    }
    if basic_smart {
        keys.insert("basic-smart".into());
    }
    if let Some(issue) = telemetry_issue {
        keys.insert(format!("telemetry-status:{issue}"));
    }

    let mut details = health_detail_messages(health, telemetry)
        .into_iter()
        .map(|(message, _)| message)
        .collect::<Vec<_>>();
    if basic_smart {
        details.push("Leitura SMART básica; o escore tem baixa confiança".into());
    }
    if let Some(issue) = telemetry_issue
        && !details.iter().any(|message| message == issue)
    {
        details.push(issue.into());
    }
    if details.is_empty() && health.score <= 50 && health.outcome == HealthOutcome::Pass {
        details.push(format!("Saúde em nível reduzido ({}%)", health.score));
    }
    if details.is_empty() && health.outcome == HealthOutcome::Unknown {
        details.push(
            "Índice indisponível: não há evidência suficiente para calcular uma porcentagem".into(),
        );
    }
    details.sort();
    details.dedup();

    let severity = if health.outcome == HealthOutcome::Fail {
        NotificationSeverity::Critical
    } else if !keys.is_empty() {
        NotificationSeverity::Warning
    } else {
        NotificationSeverity::Info
    };
    (keys, details, severity)
}

fn ata_threshold_messages(telemetry: &Telemetry, near: bool) -> Option<Vec<String>> {
    let QueryState::Success(ata) = &telemetry.ata else {
        return None;
    };
    let messages = ata
        .attributes
        .iter()
        .filter_map(|attribute| {
            if crate::ata::unique_attribute(ata, attribute.id).is_none()
                || !(1..=254).contains(&attribute.current)
            {
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

fn relevant_telemetry_issue(kind: DiskKind, telemetry: &Telemetry) -> Option<&'static str> {
    let issue = match kind {
        DiskKind::Nvme => query_state_issue(&telemetry.nvme),
        DiskKind::Hdd | DiskKind::Ssd => query_state_issue(&telemetry.ata),
        DiskKind::Unknown if matches!(&telemetry.nvme, QueryState::Success(_)) => {
            query_state_issue(&telemetry.nvme)
        }
        DiskKind::Unknown if matches!(&telemetry.ata, QueryState::Success(ata) if !ata.attributes.is_empty()) => {
            query_state_issue(&telemetry.ata)
        }
        DiskKind::Unknown => None,
    };

    if issue.is_some() {
        return issue;
    }

    // The overall-failure query is an optional coarse signal. Its unsupported
    // state is expected on some drivers; actual errors remain visible.
    let coarse_issue = match &telemetry.overall_failure {
        QueryState::Failed { message } if is_permission_error(message) => {
            Some("Permissão insuficiente para consultar o status geral de saúde")
        }
        QueryState::Failed { .. } => Some("Falha ao consultar o status geral de saúde"),
        QueryState::NotQueried | QueryState::Unsupported | QueryState::Success(_) => None,
    };
    if coarse_issue.is_some() {
        return coarse_issue;
    }

    if kind == DiskKind::Unknown
        && !matches!(&telemetry.nvme, QueryState::Success(_))
        && !matches!(&telemetry.ata, QueryState::Success(ata) if !ata.attributes.is_empty())
    {
        if failed_permission(&telemetry.nvme) || failed_permission(&telemetry.ata) {
            return Some("Permissão insuficiente para consultar os dados de saúde");
        }
        if matches!(&telemetry.nvme, QueryState::Failed { .. })
            || matches!(&telemetry.ata, QueryState::Failed { .. })
        {
            return Some("Falha ao consultar os dados de saúde do dispositivo");
        }
        if matches!(&telemetry.nvme, QueryState::Unsupported)
            && matches!(&telemetry.ata, QueryState::Unsupported)
        {
            return Some("Leitura de saúde não compatível com o dispositivo");
        }
        if matches!(&telemetry.nvme, QueryState::NotQueried)
            || matches!(&telemetry.ata, QueryState::NotQueried)
        {
            return Some("Dados de saúde indisponíveis");
        }
    }

    None
}

fn query_state_issue<T>(state: &QueryState<T>) -> Option<&'static str> {
    match state {
        QueryState::NotQueried => Some("Dados do protocolo de saúde não consultados"),
        QueryState::Unsupported => Some("Leitura de saúde não compatível com o protocolo esperado"),
        QueryState::Failed { message } if is_permission_error(message.as_str()) => {
            Some("Permissão insuficiente para a consulta de saúde relevante")
        }
        QueryState::Failed { .. } => Some("Falha ao consultar a telemetria de saúde relevante"),
        QueryState::Success(_) => None,
    }
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
    paint_background_with_attributes(ops, x, y, text, color, bold, false, max_width, background);
}

#[allow(clippy::too_many_arguments)]
fn paint_background_with_attributes(
    ops: &mut RenderOpIRVec,
    x: usize,
    y: usize,
    text: &str,
    color: RgbValue,
    bold: bool,
    italic: bool,
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
            italic: italic.then_some(r3bl_tui::tui_style_attrib::Italic),
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

fn display_width(text: &str) -> usize {
    r3bl_tui::GCStringOwned::new(text)
        .display_width()
        .as_usize()
}

fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0;
    for word in text.split_whitespace() {
        let mut remaining = word.to_owned();
        while display_width(&remaining) > max_width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_width = 0;
            }
            let chunk = clipped(&remaining, max_width);
            if chunk.is_empty() {
                break;
            }
            remaining = remaining[chunk.len()..].to_owned();
            lines.push(chunk);
        }
        if remaining.is_empty() {
            continue;
        }
        let word_width = display_width(&remaining);
        if !current.is_empty() && current_width + 1 + word_width > max_width {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        if !current.is_empty() {
            current.push(' ');
            current_width += 1;
        }
        current.push_str(&remaining);
        current_width += word_width;
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
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

    #[test]
    fn bold_and_italic_styles_reach_the_terminal_ansi_renderer() {
        let mut ops = RenderOpIRVec::new();
        paint_background_with_attributes(
            &mut ops,
            0,
            0,
            "SAÚDE",
            TOKYO_GREEN,
            true,
            false,
            8,
            Some(TOKYO_BG),
        );
        paint_background_with_attributes(
            &mut ops,
            0,
            1,
            "ETA",
            TOKYO_FG_DARK,
            false,
            true,
            3,
            Some(TOKYO_BG),
        );
        let mut pipeline = RenderPipeline::default();
        pipeline.push(ZOrder::Normal, ops);
        let size = r3bl_tui::Size::new((r3bl_tui::ColWidth::new(10), r3bl_tui::RowHeight::new(2)));
        let mut buffer = r3bl_tui::OffscreenBuffer::new_empty(size);
        pipeline.compose_render_ops_into_ofs_buf(
            size,
            &mut buffer,
            &mut r3bl_tui::MemoizedLenMap::default(),
        );
        let ansi = String::from_utf8(r3bl_tui::RenderToAnsi::render_to_ansi(&buffer)).unwrap();
        assert!(
            ansi.contains("\u{1b}[1"),
            "renderer omitted bold SGR: {ansi:?}"
        );
        assert!(
            ansi.contains("\u{1b}[3"),
            "renderer omitted italic SGR: {ansi:?}"
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

    fn fixture_buffer(state: Dashboard, width: usize, height: usize) -> r3bl_tui::OffscreenBuffer {
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
        buffer
    }

    fn append_fixture_cells(
        html: &mut String,
        label: &str,
        width: usize,
        height: usize,
        buffer: &r3bl_tui::OffscreenBuffer,
    ) {
        html.push_str(&format!(
            "<section><div class=\"case\">{label} · {width} × {height}</div><div class=\"viewport\">"
        ));
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
                    panic!("unpainted cell in {label} at {width}x{height}: {cell:?}");
                };
                let fg = style
                    .color_fg
                    .map(rgb_hex)
                    .expect("cell without explicit foreground");
                let bg = style
                    .color_bg
                    .map(rgb_hex)
                    .expect("cell without explicit background");
                assert!(is_tokyo_moon_color(style.color_fg.unwrap()));
                assert!(is_tokyo_moon_color(style.color_bg.unwrap()));
                let glyph_width = r3bl_tui::GCStringOwned::width_char(*display_char)
                    .as_usize()
                    .max(1);
                if glyph_width == 2 && matches!(cell_iter.peek(), Some(r3bl_tui::PixelChar::Void)) {
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
                let font_weight = if style.attribs.bold.is_some() {
                    "bold"
                } else {
                    "normal"
                };
                let font_style = if style.attribs.italic.is_some() {
                    "italic"
                } else {
                    "normal"
                };
                html.push_str(&format!("<span class=\"{cell_class}\" style=\"color:{fg};background:{bg};font-weight:{font_weight};font-style:{font_style}\">{ch}</span>"));
            }
            html.push_str("</div>");
        }
        html.push_str("</div></section>");
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
            "<!doctype html><meta charset=\"utf-8\"><title>chkdsk · Tokyo Night Moon</title><style>body{margin:24px;background:#222436;color:#C8D3F5;font:14px system-ui,sans-serif}h1{font-size:18px;font-weight:600;margin:0 0 20px}section{margin:0 0 30px}.case{color:#828BB8;margin:8px 0 5px}.viewport{overflow:auto;display:inline-block;background:#222436;padding:2px}.row{display:grid;grid-auto-rows:16px}.cell{display:block;width:8px;height:16px;padding:0;margin:0;overflow:hidden;font-family:\"Cascadia Mono\",\"Cascadia Code\",Consolas,monospace;font-size:13px;line-height:16px;letter-spacing:0;font-kerning:none;white-space:pre}.cell.wide{grid-column:span 2;width:16px}</style><h1>chkdsk · Tokyo Night Moon · renderização exata de células com peso/estilo</h1>",
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
                        let font_weight = if style.attribs.bold.is_some() {
                            "bold"
                        } else {
                            "normal"
                        };
                        let font_style = if style.attribs.italic.is_some() {
                            "italic"
                        } else {
                            "normal"
                        };
                        html.push_str(&format!("<span class=\"{cell_class}\" style=\"color:{fg};background:{bg};font-weight:{font_weight};font-style:{font_style}\">{ch}</span>"));
                    }
                    html.push_str("</div>");
                }
                html.push_str("</div></section>");
            }
        }
        // Synthetic interaction fixtures supplement the disk-only viewport
        // cases with a real rendered toast and both full/compact center views.
        let mut nvme_telemetry = Telemetry::default();
        nvme_telemetry.apply_nvme(crate::model::NvmeHealth {
            percentage_used: Some(4),
            ..Default::default()
        });
        let mut nvme_identity = identity("fixture-xpg", 0);
        nvme_identity.model = Some("XPG GAMMIX S41".into());
        nvme_identity.bus = Some("NVMe".into());
        nvme_identity.kind = DiskKind::Nvme;
        let hdd_telemetry = Telemetry {
            ata: QueryState::Success(crate::model::AtaHealth {
                attributes: vec![crate::model::AtaAttribute {
                    id: 197,
                    flags: 0,
                    current: 100,
                    worst: 100,
                    threshold: None,
                    raw: [1, 0, 0, 0, 0, 0],
                }],
                thresholds_available: false,
            }),
            ata_profile: Some(crate::ata::WD10EZEX_PROFILE.to_owned()),
            ..Default::default()
        };
        let mut hdd_identity = identity("fixture-wdc", 1);
        hdd_identity.model = Some("WDC WD10EZEX-00WN4A0".into());
        hdd_identity.bus = Some("SATA".into());
        hdd_identity.kind = DiskKind::Hdd;
        let (_, hdd_details, hdd_severity) = health_notification_snapshot(
            DiskKind::Hdd,
            &score_health(DiskKind::Hdd, &hdd_telemetry),
            &hdd_telemetry,
        );
        let mut notification_state = Dashboard {
            disks: vec![
                DiskView {
                    identity: nvme_identity,
                    telemetry: Some(nvme_telemetry),
                    querying: false,
                    computing: false,
                    pending_telemetry: None,
                    error_message: None,
                },
                DiskView {
                    identity: hdd_identity,
                    telemetry: Some(hdd_telemetry),
                    querying: false,
                    computing: false,
                    pending_telemetry: None,
                    error_message: None,
                },
            ],
            generation: 1,
            ..Default::default()
        };
        push_notification(
            &mut notification_state,
            Some("fixture-wdc".into()),
            "Atenção à saúde · Disco #1".into(),
            hdd_details,
            hdd_severity,
            Instant::now(),
        );
        for (label, open, width, height) in [
            ("fixture sintético · toast", false, 100, 28),
            ("fixture sintético · central", true, 100, 28),
            ("fixture sintético · central compacta", true, 30, 10),
        ] {
            notification_state.notifications_open = open;
            let buffer = fixture_buffer(notification_state.clone(), width, height);
            append_fixture_cells(&mut html, label, width, height, &buffer);
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
            let layout = Layout::for_state(width, height, &global.state);
            assert_eq!(global.state.selected, 8);
            if width == 100 {
                assert_eq!(
                    global.state.scroll,
                    8usize.saturating_sub(layout.tab_indices.len().saturating_sub(1))
                );
                assert_eq!(layout.tab_indices.last(), Some(&8));
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
                    .contains("Disco #8"),
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
        let visible = Layout::for_state(100, 28, &state);
        assert_eq!(
            visible.tab_indices,
            (0..visible.tabs.len()).collect::<Vec<_>>()
        );
        let second_tab = visible.tabs[1];
        let second_y = second_tab.y + second_tab.height / 2;
        assert_eq!(visible.tab_at(second_tab.x, second_y), Some(1));
        assert_eq!(
            visible.tab_at(second_tab.x + second_tab.width - 1, second_y),
            Some(1)
        );
        assert_eq!(
            visible.tab_at(second_tab.x + second_tab.width, second_y),
            None
        );
        state.selected = 8;
        ensure_selected_visible(&mut state, 100, 28);
        let shifted = Layout::for_state(100, 28, &state);
        assert_eq!(shifted.tab_indices.last(), Some(&8));
        assert!(shifted.tab_indices.contains(&8));
        let first_shifted = shifted.tabs[0];
        assert_eq!(
            shifted.tab_at(first_shifted.x, first_shifted.y),
            shifted.tab_indices.first().copied()
        );

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
        let hover_layout = Layout::for_state(100, 28, &global.state);
        let hover_area = hover_layout.tabs[1];
        let hover_x = hover_area.x + hover_area.width / 2;
        let hover_y = hover_area.y + hover_area.height / 2;
        assert!(matches!(
            dispatch_input(
                &mut app,
                &mut global,
                mouse_event(MouseInputKind::MouseMove, hover_x, hover_y)
            ),
            EventPropagation::ConsumedRender
        ));
        assert_eq!(global.state.hovered_tab, Some(hover_layout.tab_indices[1]));
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
            matches!(buffer[hover_y][hover_x], r3bl_tui::PixelChar::PlainText { style, .. } if style.color_bg == Some(TuiColor::Rgb(TOKYO_BG_HIGHLIGHT)))
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

        assert_eq!(
            layout_for_state(100, 10, &global.state).tab_at(20, 1),
            Some(5)
        );
        dispatch_input(
            &mut app,
            &mut global,
            mouse_event(MouseInputKind::MouseDown(Button::Left), 20, 1),
        );
        assert_eq!(
            global.state.disks[global.state.selected].identity.id,
            "disk-5"
        );
        dispatch_input(
            &mut app,
            &mut global,
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('d'),
            }),
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
            layout_for_state(100, 10, &global.state)
                .tab_indices
                .contains(&global.state.selected)
        );
        let rendered = rendered_text(&mut app, &mut global);
        let layout = layout_for_state(100, 10, &global.state);
        assert!(
            rendered
                .lines()
                .nth(layout.tabs[0].y + layout.tabs[0].height / 2)
                .unwrap()
                .contains("Disco #5")
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
            layout_for_state(100, 10, &global.state)
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
            ("ATENÇÃO · SINAIS SMART", TOKYO_YELLOW)
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
    fn hdd_counter_warning_variants_have_clear_brazilian_portuguese_details() {
        let health = crate::health::HealthScore {
            score: 60,
            outcome: HealthOutcome::Pass,
            confidence: 0.8,
            provenance: vec![],
            warnings: vec![
                HealthWarningReason::AtaReallocatedSectors,
                HealthWarningReason::AtaSpinRetries,
                HealthWarningReason::AtaReportedUncorrectable,
                HealthWarningReason::AtaPendingSectors,
                HealthWarningReason::AtaOfflineUncorrectable,
            ],
        };
        let details = health_detail_messages(&health, &Telemetry::default());
        let messages = details
            .iter()
            .map(|(text, _)| text.as_str())
            .collect::<Vec<_>>();
        for expected in [
            "Setores realocados registrados na mídia",
            "Tentativas repetidas de giro registradas",
            "Erros incorrigíveis reportados pelo dispositivo",
            "Setores pendentes aguardando remapeamento",
            "Setores incorrigíveis encontrados em verificações offline",
        ] {
            assert!(
                messages.contains(&expected),
                "missing {expected:?}: {messages:?}"
            );
        }
        assert!(details.iter().all(|(_, color)| *color == TOKYO_YELLOW));
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
    fn failure_details_ignore_ambiguous_duplicate_ata_ids() {
        let telemetry = Telemetry {
            overall_failure: QueryState::Success(true),
            ata: QueryState::Success(crate::model::AtaHealth {
                attributes: [5, 5]
                    .into_iter()
                    .map(|id| crate::model::AtaAttribute {
                        id,
                        flags: 1,
                        current: 5,
                        worst: 5,
                        threshold: Some(10),
                        raw: [0; 6],
                    })
                    .collect(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let health = score_health(DiskKind::Hdd, &telemetry);

        assert_eq!(health.outcome, HealthOutcome::Fail);
        assert_eq!(
            health_detail_messages(&health, &telemetry),
            vec![("Falha geral reportada pelo dispositivo".into(), TOKYO_RED)]
        );
    }

    #[test]
    fn nvme_log_restores_unknown_kind_without_requiring_optional_wear() {
        let mut disk_identity = identity("nvme-partial", 0);
        let mut telemetry = Telemetry::default();
        telemetry.apply_nvme(crate::model::NvmeHealth::default());
        assert_eq!(telemetry.endurance_percent, None);
        assert_eq!(
            effective_disk_kind(&disk_identity, &telemetry),
            DiskKind::Nvme
        );
        let mut history = History::default();
        history
            .record(HealthSample {
                disk_id: disk_identity.id.clone(),
                timestamp_unix_secs: 1_000,
                kind: effective_disk_kind(&disk_identity, &telemetry),
                endurance_percent: telemetry.endurance_percent,
                power_on_hours: telemetry.power_on_hours,
                score: None,
            })
            .unwrap();
        assert_eq!(history.samples[0].endurance_percent, None);
        assert_eq!(history.estimate_endurance(&disk_identity.id), None);

        for kind in [DiskKind::Hdd, DiskKind::Ssd, DiskKind::Nvme] {
            disk_identity.kind = kind;
            assert_eq!(effective_disk_kind(&disk_identity, &telemetry), kind);
        }
        disk_identity.kind = DiskKind::Unknown;
        for state in [
            QueryState::NotQueried,
            QueryState::Unsupported,
            QueryState::Failed {
                message: "invalid NVMe log".into(),
            },
        ] {
            telemetry.nvme = state;
            // Common wear alone does not establish the protocol or media kind.
            telemetry.endurance_percent = Some(12.0);
            assert_eq!(
                effective_disk_kind(&disk_identity, &telemetry),
                DiskKind::Unknown
            );
        }
    }

    #[test]
    fn nvme_query_restores_unknown_kind_history_and_rendered_eta() {
        let disk_identity = identity("nvme-fallback", 0);
        assert_eq!(disk_identity.kind, DiskKind::Unknown);
        assert_eq!(disk_identity.bus, None);
        let mut telemetry = Telemetry::default();
        telemetry.apply_nvme(crate::model::NvmeHealth {
            percentage_used: Some(12),
            ..Default::default()
        });
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
                    pending_telemetry: Some(telemetry),
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
                kind: DiskKind::Nvme,
                endurance_percent: Some(11.0),
                power_on_hours: None,
                score: Some(89),
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
        let sample = global.state.history.samples.last().unwrap();
        assert_eq!(sample.kind, DiskKind::Nvme);
        assert_eq!(sample.score, Some(88));
        assert_eq!(sample.endurance_percent, Some(12.0));
        assert!(
            global
                .state
                .history
                .estimate_endurance(&disk_identity.id)
                .is_some()
        );
        let rendered = rendered_text(&mut app, &mut global);
        assert!(rendered.contains("88%"), "{rendered}");
        assert!(rendered.contains("ETA restante"), "{rendered}");
        // Discovery identity stays stable for scheduler/hotplug comparisons.
        assert_eq!(global.state.disks[0].identity, disk_identity);
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
        for (width, height) in [
            (0, 0),
            (1, 1),
            (4, 2),
            (12, 6),
            (80, 17),
            (80, 24),
            (100, 10),
        ] {
            let layout = Layout::new(width, height, 100, 20);
            let areas = layout
                .tabs
                .iter()
                .copied()
                .chain(layout.controls.iter().map(|control| control.area))
                .chain(std::iter::once(layout.notification_button))
                .chain(std::iter::once(layout.quit))
                .chain(std::iter::once(layout.toast))
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
        assert_eq!(layout.quit.height, 1);
        assert_eq!(layout.tabs.len(), 2);
        assert_eq!(layout.tabs[0].height, 1);
        assert_eq!(body_view_height(&layout, 10), 8);
        let shorter = Layout::new(80, 14, 2, 0);
        assert!(shorter.controls.is_empty());
        assert_eq!(body_view_height(&shorter, 14), 8);
    }

    #[test]
    fn standard_buttons_are_padded_centered_and_have_full_hitboxes() {
        for (width, height) in [(100, 28), (70, 20), (80, 17), (30, 11)] {
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
            if layout.notification_button.visible() && width >= 36 {
                buttons.push((layout.notification_button, "Avisos 0".to_owned()));
            }
            buttons.extend(layout.tabs.iter().enumerate().map(|(position, area)| {
                let index = layout.tab_indices[position];
                let marker = if index == 0 { "> " } else { "  " };
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
                assert!(text_x > area.x, "{width}x{height}: {area:?}");
                assert!(
                    text_x + text_width < area.x + area.width,
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
    fn temporary_notices_expire_but_remain_available_to_keyboard_and_mouse() {
        let mut notice_identity = identity("notice-disk", 0);
        notice_identity.kind = DiskKind::Nvme;
        let mut notice_telemetry = Telemetry::default();
        notice_telemetry.apply_nvme(crate::model::NvmeHealth {
            percentage_used: Some(12),
            ..Default::default()
        });
        let mut state = Dashboard {
            disks: vec![DiskView {
                identity: notice_identity,
                telemetry: Some(notice_telemetry),
                querying: false,
                computing: false,
                pending_telemetry: None,
                error_message: None,
            }],
            generation: 7,
            ..Default::default()
        };
        push_notification(
            &mut state,
            Some("notice-disk".into()),
            "Atenção à saúde · Disco #0".into(),
            (0..20)
                .map(|index| format!("Detalhe SMART {index}"))
                .collect(),
            NotificationSeverity::Warning,
            Instant::now(),
        );
        let mut app = DashboardApp {
            started: true,
            ..Default::default()
        };
        let mut global = test_global_data(state, 100, 17);

        // The active toast is a full-row mouse target and opens the center.
        let layout = layout_for_state(100, 17, &global.state);
        let toast = rendered_text(&mut app, &mut global);
        assert!(
            toast.contains("Novo aviso · N ou clique para abrir"),
            "{toast}"
        );
        dispatch_input(
            &mut app,
            &mut global,
            mouse_event(
                MouseInputKind::MouseDown(Button::Left),
                layout.toast.x,
                layout.toast.y,
            ),
        );
        assert!(global.state.notifications_open);
        let center = rendered_text(&mut app, &mut global);
        assert!(center.contains("AVISOS"), "{center}");
        assert!(center.contains("Fechar"), "{center}");
        assert!(center.contains("↑"), "{center}");
        assert!(center.contains("↓"), "{center}");
        assert!(center.contains("Detalhe SMART 0"), "{center}");
        let center_layout = layout_for_state(100, 17, &global.state);

        // Mouse scrolling and keyboard scrolling operate on the same history.
        dispatch_input(
            &mut app,
            &mut global,
            mouse_event(
                MouseInputKind::ScrollDown,
                center_layout.panel.x + 1,
                center_layout.panel.y + 3,
            ),
        );
        assert_eq!(global.state.notification_scroll, 1);
        dispatch_input(
            &mut app,
            &mut global,
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::PageDown),
            }),
        );
        let _ = rendered_text(&mut app, &mut global);
        assert!(global.state.notification_scroll > 1);
        let center_layout = layout_for_state(100, 17, &global.state);
        let scroll_button = center_layout
            .controls
            .iter()
            .find(|control| control.action == FooterAction::ScrollDown)
            .unwrap();
        assert_eq!(scroll_button.label, "↓");
        let scroll_before_button = global.state.notification_scroll;
        dispatch_input(
            &mut app,
            &mut global,
            mouse_event(
                MouseInputKind::MouseDown(Button::Left),
                scroll_button.area.x + scroll_button.area.width / 2,
                scroll_button.area.y + scroll_button.area.height / 2,
            ),
        );
        assert_eq!(global.state.notification_scroll, scroll_before_button + 1);

        // A body click is consumed by the open center and cannot scroll the
        // dashboard content hidden underneath it.
        global.state.content_scroll = 0;
        dispatch_input(
            &mut app,
            &mut global,
            mouse_event(
                MouseInputKind::MouseDown(Button::Left),
                center_layout.panel.x + 1,
                center_layout.panel.y + 4,
            ),
        );
        assert_eq!(global.state.content_scroll, 0);

        // The header's full notification hitbox closes it, and Escape is
        // intercepted here rather than by the global terminal exit keys.
        dispatch_input(
            &mut app,
            &mut global,
            mouse_event(
                MouseInputKind::MouseDown(Button::Left),
                layout.notification_button.x,
                layout.notification_button.y,
            ),
        );
        assert!(!global.state.notifications_open);
        dispatch_input(
            &mut app,
            &mut global,
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('n'),
            }),
        );
        assert!(global.state.notifications_open);
        assert!(matches!(
            dispatch_input(
                &mut app,
                &mut global,
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Esc),
                }),
            ),
            EventPropagation::ConsumedRender
        ));
        assert!(!global.state.notifications_open);

        // Auto-hide clears only the toast. The current-generation history is
        // still searchable and the next frame repaints the cleared toast row.
        global.state.toast_was_active = true;
        global.state.notifications[0].expires_at = Instant::now() - Duration::from_millis(1);
        assert!(active_notification(&global.state, Instant::now()).is_none());
        assert!(
            notification_center_lines(&global.state, 98)
                .iter()
                .any(|line| line.text.contains("Atenção à saúde"))
        );
        assert!(matches!(
            dispatch_signal(&mut app, &mut global, AppSignal::Frame),
            EventPropagation::ConsumedRender
        ));
        dispatch_input(
            &mut app,
            &mut global,
            mouse_event(
                MouseInputKind::MouseDown(Button::Left),
                layout.notification_button.x,
                layout.notification_button.y,
            ),
        );
        assert!(global.state.notifications_open);
        assert!(rendered_text(&mut app, &mut global).contains("Detalhe SMART 0"));
        dispatch_input(
            &mut app,
            &mut global,
            mouse_event(
                MouseInputKind::MouseDown(Button::Left),
                layout.notification_button.x,
                layout.notification_button.y,
            ),
        );
        assert!(!global.state.notifications_open);
        dispatch_input(
            &mut app,
            &mut global,
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('n'),
            }),
        );
        assert!(rendered_text(&mut app, &mut global).contains("Detalhe SMART 0"));
    }

    #[test]
    fn compact_notification_center_wraps_long_warnings_and_mouse_can_read_to_the_end() {
        let mut state = Dashboard {
            generation: 2,
            ..Default::default()
        };
        push_notification(
            &mut state,
            None,
            "Atenção à saúde".into(),
            vec![
                "Setores pendentes aguardando remapeamento".into(),
                "A lista de avisos permanece nesta geração".into(),
                "Último aviso completo".into(),
            ],
            NotificationSeverity::Warning,
            Instant::now(),
        );
        let mut app = DashboardApp {
            started: true,
            ..Default::default()
        };
        let mut global = test_global_data(state, 30, 17);
        let layout = layout_for_state(30, 17, &global.state);
        assert!(layout.notification_button.visible());
        dispatch_input(
            &mut app,
            &mut global,
            mouse_event(
                MouseInputKind::MouseDown(Button::Left),
                layout.notification_button.x,
                layout.notification_button.y,
            ),
        );
        assert!(global.state.notifications_open);
        let panel_layout = layout_for_state(30, 17, &global.state);
        let first_page = rendered_text(&mut app, &mut global);
        let first_page_words = first_page.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(first_page.contains("AVISOS"), "{first_page}");
        assert!(
            first_page_words.contains("Setores pendentes aguardando remapeamento"),
            "a frase deve ser refluída sem truncamento: {first_page}"
        );
        for _ in 0..8 {
            dispatch_input(
                &mut app,
                &mut global,
                mouse_event(
                    MouseInputKind::ScrollDown,
                    panel_layout.panel.x + 1,
                    panel_layout.panel.y + 3,
                ),
            );
        }
        let last_page = rendered_text(&mut app, &mut global);
        assert!(last_page.contains("Último aviso completo"), "{last_page}");
        dispatch_input(
            &mut app,
            &mut global,
            mouse_event(
                MouseInputKind::MouseDown(Button::Left),
                layout.notification_button.x,
                layout.notification_button.y,
            ),
        );
        assert!(!global.state.notifications_open);
    }

    #[test]
    fn hardware_generation_replaces_collection_notices_preserves_selection_and_rejects_late_results()
     {
        let keep = identity("stable-drive-id", 0);
        let remove = identity("removed-drive-id", 1);
        let mut state = Dashboard {
            disks: [keep.clone(), remove.clone()]
                .into_iter()
                .map(|identity| DiskView {
                    identity,
                    telemetry: None,
                    querying: false,
                    computing: false,
                    pending_telemetry: None,
                    error_message: None,
                })
                .collect(),
            selected: 0,
            generation: 5,
            inventory_initialized: true,
            notifications_open: true,
            ..Default::default()
        };
        push_notification(
            &mut state,
            Some(keep.id.clone()),
            "Aviso antigo".into(),
            vec!["Resultado da geração anterior".into()],
            NotificationSeverity::Critical,
            Instant::now(),
        );
        state.notifications_open = true;
        let mut app = DashboardApp {
            started: true,
            ..Default::default()
        };
        let mut global = test_global_data(state, 100, 28);
        let added = identity("new-drive-id", 2);
        assert!(matches!(
            dispatch_signal(
                &mut app,
                &mut global,
                AppSignal::Discovered {
                    generation: 6,
                    result: Ok(vec![keep.clone(), added.clone()]),
                },
            ),
            EventPropagation::ConsumedRender
        ));
        assert_eq!(global.state.selected, 0);
        assert_eq!(
            global.state.disks[global.state.selected].identity.id,
            keep.id
        );
        assert!(global.state.notifications_open);
        assert!(
            global
                .state
                .notifications
                .iter()
                .all(|notice| notice.title != "Aviso antigo")
        );
        assert_eq!(global.state.notifications.len(), 1);
        assert_eq!(
            global.state.notifications[0].generation,
            global.state.collection_generation
        );
        assert!(
            global.state.notifications[0]
                .details
                .iter()
                .any(|text| text.contains("Conectado"))
        );
        assert!(
            global.state.notifications[0]
                .details
                .iter()
                .any(|text| text.contains("Removido"))
        );

        // Both an old query completion and delayed compute are ignored.
        global.state.disks[0].computing = true;
        global.state.disks[0].pending_telemetry = Some(Telemetry::default());
        dispatch_signal(
            &mut app,
            &mut global,
            AppSignal::QueryFinished {
                generation: 5,
                disk_id: keep.id.clone(),
                result: Err(QueryErrorState::Transient),
            },
        );
        dispatch_signal(
            &mut app,
            &mut global,
            AppSignal::ComputeFinished {
                generation: 5,
                disk_id: keep.id.clone(),
            },
        );
        assert!(global.state.disks[0].computing);
        assert_eq!(global.state.notifications.len(), 1);

        // A current-generation result can notify again immediately.
        let warning_telemetry = Telemetry {
            nvme: QueryState::Success(crate::model::NvmeHealth {
                critical_warning: 1,
                ..Default::default()
            }),
            ..Default::default()
        };
        global.state.disks[0].pending_telemetry = Some(warning_telemetry);
        dispatch_signal(
            &mut app,
            &mut global,
            AppSignal::ComputeFinished {
                generation: 6,
                disk_id: keep.id.clone(),
            },
        );
        assert_eq!(global.state.notifications.len(), 2);
        assert_eq!(
            global.state.notifications[1].generation,
            global.state.collection_generation
        );
        assert_eq!(
            global.state.notifications[1].severity,
            NotificationSeverity::Critical
        );
        assert!(active_notification(&global.state, Instant::now()).is_some());

        let center_view = rendered_text(&mut app, &mut global);
        assert!(
            center_view.contains("Reserva NVMe em nível crítico"),
            "{center_view}"
        );
        dispatch_input(
            &mut app,
            &mut global,
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('n'),
            }),
        );
        assert!(!global.state.notifications_open);
        for notice in &mut global.state.notifications {
            notice.expires_at = Instant::now() - Duration::from_millis(1);
        }
        global.state.toast_was_active = true;
        let _ = dispatch_signal(&mut app, &mut global, AppSignal::Frame);
        let main_view = rendered_text(&mut app, &mut global);
        assert!(main_view.contains("25%"), "{main_view}");
        assert!(
            !main_view.contains("Reserva NVMe em nível crítico"),
            "detalhe deve ficar fora do resumo: {main_view}"
        );
        dispatch_input(
            &mut app,
            &mut global,
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('n'),
            }),
        );
        let center_view = rendered_text(&mut app, &mut global);
        assert!(
            center_view.contains("Reserva NVMe em nível crítico"),
            "aviso detalhado acessível na central: {center_view}"
        );

        // Identical routine polls do not create duplicate toasts; a recovery
        // clears the active issue key so a later recurrence can notify again.
        global.state.disks[0].pending_telemetry = Some(Telemetry {
            nvme: QueryState::Success(crate::model::NvmeHealth::default()),
            ..Default::default()
        });
        dispatch_signal(
            &mut app,
            &mut global,
            AppSignal::ComputeFinished {
                generation: 6,
                disk_id: keep.id.clone(),
            },
        );
        assert_eq!(global.state.notifications.len(), 1);
        global.state.disks[0].pending_telemetry = Some(Telemetry {
            nvme: QueryState::Success(crate::model::NvmeHealth {
                critical_warning: 1,
                ..Default::default()
            }),
            ..Default::default()
        });
        dispatch_signal(
            &mut app,
            &mut global,
            AppSignal::ComputeFinished {
                generation: 6,
                disk_id: keep.id,
            },
        );
        assert_eq!(global.state.notifications.len(), 2);
    }

    #[test]
    fn failed_inventory_refresh_keeps_the_last_snapshot_and_reports_the_failure() {
        let identities = [identity("drive-a", 0), identity("drive-b", 1)];
        let state = Dashboard {
            disks: identities
                .iter()
                .cloned()
                .map(|identity| DiskView {
                    identity,
                    telemetry: None,
                    querying: false,
                    computing: false,
                    pending_telemetry: None,
                    error_message: None,
                })
                .collect(),
            generation: 3,
            inventory_initialized: true,
            selected: 1,
            ..Default::default()
        };
        let mut app = DashboardApp {
            started: true,
            ..Default::default()
        };
        let mut global = test_global_data(state, 100, 28);
        for _ in 0..2 {
            dispatch_signal(
                &mut app,
                &mut global,
                AppSignal::Discovered {
                    generation: 3,
                    result: Err("falha de enumeração".into()),
                },
            );
        }
        assert_eq!(global.state.disks.len(), 2);
        assert_eq!(
            global.state.disks[global.state.selected].identity.id,
            "drive-b"
        );
        assert_eq!(
            global.state.notice.as_deref(),
            Some("Não foi possível localizar os discos")
        );
        assert_eq!(global.state.notifications.len(), 1);
        assert_eq!(
            global.state.notifications[0].severity,
            NotificationSeverity::Warning
        );
        assert!(
            global.state.notifications[0].details[0].contains("lista atual de discos foi mantida")
        );

        let mut empty_app = DashboardApp {
            started: true,
            ..Default::default()
        };
        let mut empty = test_global_data(
            Dashboard {
                generation: 8,
                inventory_initialized: true,
                ..Default::default()
            },
            100,
            28,
        );
        dispatch_signal(
            &mut empty_app,
            &mut empty,
            AppSignal::Discovered {
                generation: 8,
                result: Err("falha de enumeração".into()),
            },
        );
        let empty_render = rendered_text(&mut empty_app, &mut empty);
        assert!(empty_render.contains("Não foi possível localizar os discos"));
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
        let lines = disk_detail_lines(&disk, &History::default(), 64);
        let shown = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(shown.contains("Tipo: Desconhecido"));
        assert!(shown.contains("Capacidade: indisponível"));
        assert!(shown.contains("indisponível"));
        assert!(shown.contains("Dados de saúde indisponíveis"));

        let unknown = DiskView {
            telemetry: Some(Telemetry::default()),
            ..disk
        };
        let unknown_lines = disk_detail_lines(&unknown, &History::default(), 80);
        let unknown_details = unknown_lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(unknown_details.contains("SAÚDE INDISPONÍVEL"));
        assert!(!unknown_details.contains("0 / 100"));
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

        let layout = layout_for_state(80, 24, &global.state);
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
        let compact_layout = layout_for_state(12, 2, &compact.state);
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
        let tiny_layout = layout_for_state(1, 1, &tiny.state);
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
        let mut global = test_global_data(state, 80, 17);
        let mut layout = layout_for_state(80, 17, &global.state);
        let click = |app: &mut DashboardApp,
                     global: &mut GlobalData<Dashboard, AppSignal>,
                     layout: &Layout,
                     action| {
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

        click(&mut app, &mut global, &layout, FooterAction::NextDisk);
        assert_eq!(global.state.selected, 1);
        dispatch_input(
            &mut app,
            &mut global,
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Left),
            }),
        );
        assert_eq!(global.state.selected, 0);

        dispatch_input(
            &mut app,
            &mut global,
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('d'),
            }),
        );
        layout = layout_for_state(80, 17, &global.state);
        click(&mut app, &mut global, &layout, FooterAction::ScrollDown);
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
        click(&mut app, &mut global, &layout, FooterAction::ScrollUp);
        assert_eq!(global.state.content_scroll, 0);
    }

    #[test]
    fn compact_mouse_controls_cover_disk_selection_summary_scroll_and_exit() {
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
        let mut global = test_global_data(state, 30, 6);
        let layout = layout_for_state(30, 6, &global.state);
        assert!(layout.controls.is_empty());

        dispatch_input(
            &mut app,
            &mut global,
            mouse_event(
                MouseInputKind::ScrollDown,
                layout.tabs[0].x + 1,
                layout.tabs[0].y + layout.tabs[0].height / 2,
            ),
        );
        assert_eq!(global.state.selected, 1);
        assert_eq!(global.state.scroll, 0);

        let selected_layout = layout_for_state(30, 6, &global.state);
        let selected_position = selected_layout
            .tab_indices
            .iter()
            .position(|index| *index == global.state.selected)
            .unwrap();
        let selected_tab = selected_layout.tabs[selected_position];
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
        assert_eq!(global.state.content_scroll, 0);

        assert!(matches!(
            dispatch_input(
                &mut app,
                &mut global,
                mouse_event(
                    MouseInputKind::MouseDown(Button::Left),
                    layout.quit.x + 1,
                    layout.quit.y + layout.quit.height / 2,
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

        for (width, height) in [(80, 17), (30, 17)] {
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
            dispatch_input(
                &mut app,
                &mut global,
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::Character('d'),
                }),
            );
            let layout = layout_for_state(width, height, &global.state);
            let details = disk_detail_lines(
                &disk,
                &global.state.history,
                layout.panel.width.saturating_sub(2).max(1),
            );
            let visible_lines = panel_visible_lines(&layout);
            assert!(visible_lines > 0, "{width}x{height}: {layout:?}");
            let expected_max = details.len().saturating_sub(visible_lines);

            for _ in 0..30 {
                dispatch_input(
                    &mut app,
                    &mut global,
                    InputEvent::Keyboard(KeyPress::Plain {
                        key: Key::SpecialKey(SpecialKey::Down),
                    }),
                );
            }
            assert_eq!(
                global.state.content_scroll,
                expected_max,
                "{width}x{height} {layout:?}; detail_lines={} visible_lines={visible_lines}",
                details.len()
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
                    mouse_event(MouseInputKind::ScrollDown, layout.panel.x, layout.panel.y),
                );
            }
            assert_eq!(global.state.content_scroll, expected_max);
            dispatch_input(
                &mut app,
                &mut global,
                mouse_event(MouseInputKind::ScrollDown, layout.panel.x, layout.panel.y),
            );
            assert_eq!(global.state.content_scroll, expected_max);
            dispatch_input(
                &mut app,
                &mut global,
                mouse_event(MouseInputKind::ScrollUp, layout.panel.x, layout.panel.y),
            );
            assert_eq!(global.state.content_scroll, expected_max - 1);

            for _ in 0..5 {
                dispatch_input(
                    &mut app,
                    &mut global,
                    mouse_event(MouseInputKind::ScrollDown, layout.panel.x, layout.panel.y),
                );
            }
            assert_eq!(global.state.content_scroll, expected_max);
            let rendered = rendered_text(&mut app, &mut global);
            assert!(rendered.contains("Sair"), "{rendered}");
            assert!(rendered.contains("DETALHES DO DISCO"), "{rendered}");
            assert!(
                !rendered.contains("Backup da memória volátil"),
                "{rendered}"
            );
            for _ in 0..expected_max {
                dispatch_input(
                    &mut app,
                    &mut global,
                    InputEvent::Keyboard(KeyPress::Plain {
                        key: Key::SpecialKey(SpecialKey::Up),
                    }),
                );
            }
            let beginning_render = rendered_text(&mut app, &mut global);
            assert_eq!(global.state.content_scroll, 0);
            assert!(
                beginning_render.contains("DETALHES DO DISCO"),
                "{beginning_render}"
            );
            let backup_row = details
                .iter()
                .position(|line| line.text.contains("Backup da memória volátil"))
                .expect("critical-warning backup explanation should be in detail data");
            assert!(backup_row <= expected_max);
            for _ in 0..backup_row {
                dispatch_input(
                    &mut app,
                    &mut global,
                    InputEvent::Keyboard(KeyPress::Plain {
                        key: Key::SpecialKey(SpecialKey::Down),
                    }),
                );
            }
            let backup_render = rendered_text(&mut app, &mut global);
            assert_eq!(global.state.content_scroll, backup_row);
            assert!(
                backup_render.contains("Backup da memória volátil"),
                "{backup_render}"
            );
            for _ in backup_row..expected_max {
                dispatch_input(
                    &mut app,
                    &mut global,
                    InputEvent::Keyboard(KeyPress::Plain {
                        key: Key::SpecialKey(SpecialKey::Down),
                    }),
                );
            }
            let rendered = rendered_text(&mut app, &mut global);
            assert_eq!(global.state.content_scroll, expected_max);
            let last_panel_row = layout.panel.y + 3 + visible_lines - 1;
            assert!(
                rendered
                    .lines()
                    .nth(last_panel_row)
                    .unwrap()
                    .contains("útil."),
                "{rendered}"
            );

            if width == 80 {
                let scroll_before_compacting = global.state.content_scroll;
                global.window_size = r3bl_tui::Size::new((
                    r3bl_tui::ColWidth::new(width),
                    r3bl_tui::RowHeight::new(15),
                ));
                let compact_layout = layout_for_state(width, 15, &global.state);
                let compact_visible = panel_visible_lines(&compact_layout);
                let compact_max = details.len().saturating_sub(compact_visible);
                let compact_scroll = scroll_before_compacting.min(compact_max);
                let compact_resize_render = rendered_text(&mut app, &mut global);
                assert_eq!(global.state.content_scroll, compact_scroll);
                assert!(
                    compact_resize_render
                        .lines()
                        .any(|line| line.contains("DETALHES DO DISCO"))
                );

                global.state.content_scroll = compact_max;
                let compact_render = rendered_text(&mut app, &mut global);
                assert_eq!(global.state.content_scroll, compact_max);
                assert!(compact_render.contains("útil."), "{compact_render}");

                global.window_size = r3bl_tui::Size::new((
                    r3bl_tui::ColWidth::new(width),
                    r3bl_tui::RowHeight::new(height),
                ));
                let expanded_layout = layout_for_state(width, height, &global.state);
                let expanded_max = details
                    .len()
                    .saturating_sub(panel_visible_lines(&expanded_layout));
                // Removing compact scroll buttons can increase readable rows.
                // Verify the actual viewport clamp rather than assuming a
                // monotonic relationship between terminal and content height.
                let _ = rendered_text(&mut app, &mut global);
                assert_eq!(global.state.content_scroll, compact_max.min(expanded_max));
                dispatch_input(
                    &mut app,
                    &mut global,
                    InputEvent::Keyboard(KeyPress::Plain {
                        key: Key::SpecialKey(SpecialKey::PageDown),
                    }),
                );
                assert_eq!(global.state.content_scroll, expanded_max);
                let expanded_end = rendered_text(&mut app, &mut global);
                assert!(expanded_end.contains("útil."), "{expanded_end}");
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
        let health = score_health(DiskKind::Hdd, &telemetry);
        let (keys, _, _) = health_notification_snapshot(DiskKind::Hdd, &health, &telemetry);
        assert!(
            !keys.iter().any(|key| key.starts_with("telemetry-status:")),
            "expected unsupported sibling protocol must not be a notification: {keys:?}"
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
    fn expected_unsupported_sibling_protocols_do_not_alert_healthy_disks() {
        let hdd_telemetry = Telemetry {
            nvme: QueryState::Unsupported,
            ata: QueryState::Success(crate::model::AtaHealth {
                attributes: [5, 10, 187, 197, 198]
                    .into_iter()
                    .map(|id| crate::model::AtaAttribute {
                        id,
                        flags: 0,
                        current: 100,
                        worst: 100,
                        threshold: None,
                        raw: [0; 6],
                    })
                    .collect(),
                thresholds_available: false,
            }),
            ata_profile: Some(crate::ata::WD10EZEX_PROFILE.to_owned()),
            overall_failure: QueryState::Success(false),
            ..Default::default()
        };
        let nvme_telemetry = Telemetry {
            nvme: QueryState::Success(crate::model::NvmeHealth {
                percentage_used: Some(10),
                ..Default::default()
            }),
            ata: QueryState::Unsupported,
            overall_failure: QueryState::Success(false),
            ..Default::default()
        };

        for (kind, telemetry) in [
            (DiskKind::Hdd, hdd_telemetry),
            (DiskKind::Nvme, nvme_telemetry),
        ] {
            let health = score_health(kind, &telemetry);
            assert_eq!(health.outcome, HealthOutcome::Pass, "{kind:?}");
            let (keys, _, severity) = health_notification_snapshot(kind, &health, &telemetry);
            assert!(keys.is_empty(), "{kind:?}: {keys:?}");
            assert_eq!(severity, NotificationSeverity::Info, "{kind:?}");
        }
    }

    #[test]
    fn failed_relevant_protocol_warns_even_when_another_protocol_succeeds() {
        let telemetry = Telemetry {
            ata: QueryState::Failed {
                message: "device query failed".into(),
            },
            nvme: QueryState::Success(crate::model::NvmeHealth {
                percentage_used: Some(10),
                ..Default::default()
            }),
            overall_failure: QueryState::Success(false),
            ..Default::default()
        };
        let health = score_health(DiskKind::Hdd, &telemetry);
        let (keys, details, severity) =
            health_notification_snapshot(DiskKind::Hdd, &health, &telemetry);
        assert!(
            keys.contains("telemetry-status:Falha ao consultar a telemetria de saúde relevante")
        );
        assert!(
            details
                .iter()
                .any(|detail| { detail == "Falha ao consultar a telemetria de saúde relevante" })
        );
        assert_eq!(severity, NotificationSeverity::Warning);
    }

    #[test]
    fn threshold_warning_details_ignore_ambiguous_duplicate_attribute_ids() {
        let telemetry = Telemetry {
            ata: QueryState::Success(crate::model::AtaHealth {
                attributes: vec![
                    crate::model::AtaAttribute {
                        id: 5,
                        flags: 0,
                        current: 105,
                        worst: 105,
                        threshold: Some(100),
                        raw: [0; 6],
                    },
                    crate::model::AtaAttribute {
                        id: 197,
                        flags: 0,
                        current: 5,
                        worst: 5,
                        threshold: Some(10),
                        raw: [0; 6],
                    },
                    crate::model::AtaAttribute {
                        id: 197,
                        flags: 0,
                        current: 6,
                        worst: 6,
                        threshold: Some(10),
                        raw: [0; 6],
                    },
                ],
                thresholds_available: true,
            }),
            ..Default::default()
        };

        assert_eq!(
            ata_threshold_messages(&telemetry, true).unwrap(),
            vec!["SMART ATA próximo do limite: atributo 5 (105/100)".to_owned()]
        );
        assert!(ata_threshold_messages(&telemetry, false).is_none());
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
            let mut disk_identity = identity("test", 0);
            disk_identity.kind = DiskKind::Nvme;
            let state = Dashboard {
                disks: vec![DiskView {
                    identity: disk_identity,
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
            assert!(rendered.contains("N/A"), "{width}x{height}: {rendered}");
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
            80,
            17,
        );
        let rendered = rendered_text(&mut app, &mut global);
        assert!(rendered.contains("ETA —"), "{rendered}");
        assert!(!rendered.contains("Tipo: Desconhecido"), "{rendered}");
        dispatch_input(
            &mut app,
            &mut global,
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('d'),
            }),
        );
        let details = rendered_text(&mut app, &mut global);
        assert!(details.contains("Tipo: Desconhecido"), "{details}");

        global.state.disks[0].telemetry = None;
        let details = rendered_text(&mut app, &mut global);
        assert!(details.contains("Tipo: Desconhecido"), "{details}");
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
                let layout = Layout::for_state(width, height, &state);
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
                let layout = Layout::for_state(width, height, &state);
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
                            if layout.tab_indices[position] != global.state.selected
                                && matches!(buffer[y][x], r3bl_tui::PixelChar::PlainText { .. })
                            {
                                assert!(
                                    cell_has_colors(&buffer[y][x], foreground, background),
                                    "tab idx={} expected fg/bg={foreground:?}/{background:?} at ({x},{y}) pixel={:?}",
                                    layout.tab_indices[position],
                                    buffer[y][x]
                                );
                            }
                        }
                    }
                }
                let tab_row = rendered.lines().nth(layout.tabs[0].y + 1).unwrap();
                assert!(tab_row.contains("#4 MODELO-UNICO"), "{tab_row}");
                let hero_has_green_score =
                    (layout.body_y..layout.body_y + body_view_height(&layout, height)).any(|y| {
                        (0..width).any(|x| cell_has_colors(&buffer[y][x], TOKYO_GREEN, TOKYO_BG))
                    });
                assert!(hero_has_green_score, "{rendered}");
                let eta_row = rendered
                    .lines()
                    .position(|line| line.contains("ETA"))
                    .unwrap();
                assert!((0..width).any(|x| {
                    cell_has_colors(&buffer[eta_row][x], TOKYO_FG_DARK, TOKYO_BG)
                        || cell_has_colors(&buffer[eta_row][x], TOKYO_FG_DARK, TOKYO_BG_DARK)
                }));
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
                assert!(rendered.contains("88%"), "{rendered}");
                assert!(rendered.contains("ETA —"), "{rendered}");
                assert!(
                    !rendered.contains("██"),
                    "score compacto esperado: {rendered}"
                );
            }
            if width == 30 {
                let layout = layout_for_state(width, height, &global.state);
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
        let mut unknown_app = DashboardApp {
            started: true,
            ..Default::default()
        };
        let mut unknown_global = test_global_data(
            Dashboard {
                disks: vec![DiskView {
                    identity: identity("unknown-score", 0),
                    telemetry: Some(unknown),
                    querying: false,
                    computing: false,
                    pending_telemetry: None,
                    error_message: None,
                }],
                ..Default::default()
            },
            30,
            10,
        );
        let unknown_render = rendered_text(&mut unknown_app, &mut unknown_global);
        assert!(unknown_render.contains("N/A"), "{unknown_render}");
        assert!(!unknown_render.contains("0%"), "{unknown_render}");
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
    fn coarse_smart_pass_is_indeterminate_instead_of_fabricating_a_percentage() {
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
        for (width, height) in [(100, 28), (70, 20), (30, 10)] {
            let mut global = test_global_data(state.clone(), width, height);
            let rendered = rendered_text(&mut app, &mut global);
            assert!(rendered.contains("N/A"), "{width}x{height}: {rendered}");
            assert!(!rendered.contains("100%"), "{width}x{height}: {rendered}");
            assert!(
                !rendered.contains("SAUDÁVEL"),
                "{width}x{height}: {rendered}"
            );
            assert!(rendered.contains("ETA —"), "{width}x{height}: {rendered}");
        }
        let mut global = test_global_data(state, 100, 28);
        dispatch_input(
            &mut app,
            &mut global,
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('d'),
            }),
        );
        let details = rendered_text(&mut app, &mut global);
        assert!(details.contains("SAÚDE INDISPONÍVEL"), "{details}");
        assert!(details.contains("HDD"), "{details}");
    }

    #[test]
    fn ambiguous_hdd_raw_is_n_a_with_reason_available_in_notification_center() {
        let mut telemetry = Telemetry {
            overall_failure: QueryState::Success(false),
            ata: QueryState::Success(crate::model::AtaHealth {
                attributes: vec![
                    crate::model::AtaAttribute {
                        id: 5,
                        flags: 0,
                        current: 100,
                        worst: 100,
                        threshold: None,
                        raw: [1, 0, 0, 0, 1, 0],
                    },
                    crate::model::AtaAttribute {
                        id: 197,
                        flags: 0,
                        current: 100,
                        worst: 100,
                        threshold: None,
                        raw: [1, 0, 0, 0, 0, 0],
                    },
                ],
                thresholds_available: false,
            }),
            ..Default::default()
        };
        telemetry.ata_profile = Some(crate::ata::WD10EZEX_PROFILE.to_owned());
        let mut disk_identity = identity("ambiguous-hdd", 0);
        disk_identity.kind = DiskKind::Hdd;
        disk_identity.model = Some("WDC WD10EZEX-00WN4A0".into());
        let health = score_health(DiskKind::Hdd, &telemetry);
        assert_eq!(health.outcome, HealthOutcome::Unknown);
        assert_eq!(health.score, 0);
        assert!(
            health
                .warnings
                .contains(&HealthWarningReason::AtaPendingSectors)
        );
        assert!(
            health
                .warnings
                .contains(&HealthWarningReason::AtaRawEncodingAmbiguous)
        );
        let (_, details, severity) =
            health_notification_snapshot(DiskKind::Hdd, &health, &telemetry);
        assert_eq!(severity, NotificationSeverity::Warning);
        assert!(
            details
                .iter()
                .any(|text| text.contains("pendentes aguardando"))
        );
        assert!(
            details
                .iter()
                .any(|text| text.contains("Codificação bruta SMART ambígua"))
        );

        let mut state = Dashboard {
            disks: vec![DiskView {
                identity: disk_identity,
                telemetry: Some(telemetry),
                querying: false,
                computing: false,
                pending_telemetry: None,
                error_message: None,
            }],
            ..Default::default()
        };
        push_notification(
            &mut state,
            Some("ambiguous-hdd".into()),
            "Atenção à saúde · Disco #0".into(),
            details,
            severity,
            Instant::now(),
        );
        let mut app = DashboardApp {
            started: true,
            ..Default::default()
        };
        let mut global = test_global_data(state, 100, 28);
        let main_view = rendered_text(&mut app, &mut global);
        assert!(main_view.contains("N/A"), "{main_view}");
        assert!(!main_view.contains("0%"), "{main_view}");
        assert!(!main_view.contains("pendentes aguardando"), "{main_view}");
        dispatch_input(
            &mut app,
            &mut global,
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('n'),
            }),
        );
        let details_view = rendered_text(&mut app, &mut global);
        assert!(
            details_view.contains("pendentes aguardando"),
            "{details_view}"
        );
        assert!(
            details_view.contains("Codificação bruta SMART ambígua"),
            "{details_view}"
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

    include!("ui/redesign_tests.rs");
}
