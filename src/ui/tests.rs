use super::*;
use crate::{
    collector::{DiskSnapshot, Reading},
    fixtures::{self, NOW},
    model::{DiskIdentity, DiskKind},
    present::Tone,
};
use rune::{Env, Mods, Runtime, Shell};

const FRAME: f64 = 1.0 / 60.0;

fn runtime(state: Dashboard, size: (f32, f32)) -> Runtime<Dashboard> {
    let mut rt = Runtime::new(state, view, size, Env::new(true, 2.0));
    rt.on_key = Some(on_key);
    rt.frame(0.0);
    rt
}

fn settle(rt: &mut Runtime<Dashboard>) {
    for _ in 0..90 {
        rt.frame(FRAME);
    }
}

fn press(rt: &mut Runtime<Dashboard>, key: Key, mods: Mods) {
    rt.key(KeyEvent { key, mods });
    settle(rt);
}

fn plain(rt: &mut Runtime<Dashboard>, key: Key) {
    press(rt, key, Mods::default());
}

fn typed(rt: &mut Runtime<Dashboard>, text: &str) {
    plain(rt, Key::Text(text.into()));
}

fn click(rt: &mut Runtime<Dashboard>, key: &str) {
    let (x, y) = rt
        .find(key)
        .unwrap_or_else(|| panic!("{key} is not on screen"));
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    settle(rt);
}

/// Everything the window says, as the accessibility tree reports it.
fn screen(rt: &Runtime<Dashboard>) -> String {
    rt.accessibility_json().to_string()
}

fn identity(id: &str) -> DiskIdentity {
    DiskIdentity {
        id: id.into(),
        index: 0,
        model: None,
        serial: None,
        firmware: None,
        capacity_bytes: None,
        bus: None,
        kind: DiskKind::Unknown,
    }
}

fn pending_disks(count: usize) -> Dashboard {
    let mut dashboard = Dashboard::default();
    let disks = (0..count)
        .map(|index| DiskSnapshot {
            identity: identity(&index.to_string()),
            reading: Reading::Pending,
        })
        .collect();
    dashboard.apply(Update::Disks(disks), NOW);
    dashboard
}

#[test]
fn arrows_and_tab_switch_disks_and_stop_at_the_ends() {
    let mut rt = runtime(pending_disks(4), (1100.0, 720.0));
    assert_eq!(rt.state.selected, 0);
    plain(&mut rt, Key::Left);
    assert_eq!(rt.state.selected, 0, "no wrap before the first disk");
    plain(&mut rt, Key::Tab);
    assert_eq!(rt.state.selected, 1);
    plain(&mut rt, Key::Right);
    assert_eq!(rt.state.selected, 2);
    press(
        &mut rt,
        Key::Tab,
        Mods {
            shift: true,
            ..Mods::default()
        },
    );
    assert_eq!(rt.state.selected, 1, "Shift+Tab goes back");
    plain(&mut rt, Key::Left);
    assert_eq!(rt.state.selected, 0);
    for _ in 0..6 {
        plain(&mut rt, Key::Right);
    }
    assert_eq!(rt.state.selected, 3, "no wrap past the last disk");
}

#[test]
fn clicking_a_disk_selects_it() {
    let mut rt = runtime(fixtures::dashboard(0), (1100.0, 720.0));
    click(&mut rt, "disco-2");
    assert_eq!(rt.state.selected, 2);
    assert!(screen(&rt).contains("Crucial MX500 500GB"));
    click(&mut rt, "disco-1");
    assert_eq!(rt.state.selected, 1);
    assert!(screen(&rt).contains("WDC WD10EZEX-08WN4A0"));
}

#[test]
fn quit_keys_ask_the_window_to_quit() {
    for (key, mods) in [
        (Key::Text("q".into()), Mods::default()),
        (Key::Escape, Mods::default()),
    ] {
        let mut rt = runtime(fixtures::dashboard(0), (1100.0, 720.0));
        assert_eq!(rt.shell_request, None);
        press(&mut rt, key, mods);
        assert_eq!(rt.shell_request, Some(Shell::Quit));
    }
    // A shortcut that merely contains the letter does not quit.
    let mut rt = runtime(fixtures::dashboard(0), (1100.0, 720.0));
    press(
        &mut rt,
        Key::Command('q'),
        Mods {
            cmd: true,
            ctrl: true,
            ..Mods::default()
        },
    );
    typed(&mut rt, "Q");
    assert_eq!(rt.shell_request, None);
}

#[test]
fn up_down_and_the_wheel_scroll_the_panel_and_a_new_disk_starts_at_the_top() {
    // The failing disk has the long ATA table, so its panel overflows a small window.
    let mut rt = runtime(fixtures::dashboard(1), (760.0, 560.0));
    assert_eq!(rt.state.scroll.get(), 0.0);
    plain(&mut rt, Key::Down);
    let after_down = rt.state.scroll.get();
    assert!(after_down > 0.0, "Down scrolls the panel: {after_down}");
    typed(&mut rt, "j");
    assert!(rt.state.scroll.get() > after_down, "j is Down");
    plain(&mut rt, Key::Up);
    typed(&mut rt, "k");
    assert!(rt.state.scroll.get() < after_down + 1.0);
    assert_eq!(rt.state.selected, 1, "scrolling never switches disks");

    let (x, y) = rt.find("detalhe").expect("the panel is on screen");
    rt.wheel(x + 20.0, y + 20.0, -200.0);
    settle(&mut rt);
    let wheeled = rt.state.scroll.get();
    assert!(wheeled > after_down, "the wheel scrolls: {wheeled}");

    plain(&mut rt, Key::Right);
    assert_eq!(rt.state.selected, 2);
    assert_eq!(rt.state.scroll.get(), 0.0, "a new disk opens at its top");
}

#[test]
fn status_mapping_reaches_the_screen_with_its_color_tone() {
    let cases = [
        (0, "SAUDÁVEL", Some(Tone::Healthy)),
        (1, "ATENÇÃO", Some(Tone::Attention)),
        (2, "INDISPONÍVEL", Some(Tone::Unknown)),
    ];
    for (index, label, tone) in cases {
        let dashboard = fixtures::dashboard(index);
        assert_eq!(dashboard.cards[index].tone(), tone);
        let rt = runtime(dashboard, (1100.0, 720.0));
        assert!(screen(&rt).contains(label), "{label} is shown");
    }
    for (index, text) in [
        (3, "Consultando e interpretando dados…"),
        (4, "Não foi possível consultar os dados de saúde"),
    ] {
        let rt = runtime(fixtures::dashboard(index), (1100.0, 720.0));
        assert!(screen(&rt).contains(text), "{text} is shown");
    }
}

#[test]
fn unknown_health_keeps_the_lifetime_estimate_visible() {
    // The denied disk has no usable telemetry (N/A) but its history still extrapolates.
    for size in [(1100.0, 720.0), (760.0, 560.0), (640.0, 420.0)] {
        let rt = runtime(fixtures::dashboard(2), size);
        let shown = screen(&rt);
        assert!(shown.contains("N/A"), "{size:?}");
        assert!(shown.contains("Vida estimada"), "{size:?}");
        assert!(shown.contains("1246 dias"), "{size:?}: {shown}");
        assert!(!shown.contains("% saúde"), "{size:?}");
    }
}

#[test]
fn healthy_disk_shows_score_estimate_and_readings() {
    let rt = runtime(fixtures::dashboard(0), (1100.0, 720.0));
    let shown = screen(&rt);
    for text in [
        "Samsung SSD 990 PRO 1TB",
        "NVMe  ·  931,5 GiB",
        "96",
        "% saúde",
        "Vida estimada",
        "1488 dias",
        "Desgaste",
        "7%",
        "Temperatura",
        "38 °C",
        "Tempo ligado",
        "4380 h",
        "Última leitura",
        "há 1 dia",
    ] {
        assert!(shown.contains(text), "{text} is missing: {shown}");
    }
}

#[test]
fn empty_states_say_what_happened() {
    let mut dashboard = Dashboard::default();
    assert_eq!(dashboard.notice, Notice::Searching);
    let rt = runtime(Dashboard::default(), (760.0, 560.0));
    assert!(screen(&rt).contains("Procurando discos…"));

    assert!(dashboard.apply(Update::Disks(Vec::new()), NOW));
    let rt = runtime(dashboard, (760.0, 560.0));
    assert!(screen(&rt).contains("Nenhum disco encontrado"));

    let mut failed = Dashboard::default();
    assert!(failed.apply(Update::DiscoveryFailed, NOW));
    assert!(!failed.apply(Update::DiscoveryFailed, NOW));
    let rt = runtime(failed, (760.0, 560.0));
    assert!(screen(&rt).contains("Não foi possível localizar os discos"));
}

#[test]
fn updates_keep_the_selected_disk_and_report_only_visible_changes() {
    let mut dashboard = fixtures::dashboard(1);
    let selected_id = dashboard.cards[1].id.clone();
    assert!(
        !dashboard.apply(Update::Disks(fixtures::disks()), NOW),
        "same data, no redraw"
    );

    // A disk disappears above the selected one: the selection follows its id.
    let mut disks = fixtures::disks();
    disks.remove(0);
    assert!(dashboard.apply(Update::Disks(disks), NOW));
    assert_eq!(dashboard.cards[dashboard.selected].id, selected_id);

    // The selected disk disappears: back to the first, never out of range.
    let mut disks = fixtures::disks();
    disks.retain(|disk| disk.identity.id != selected_id);
    disks.truncate(1);
    dashboard.apply(Update::Disks(disks), NOW);
    assert_eq!(dashboard.selected, 0);
}

#[test]
fn fixture_text_stays_inside_the_window_at_both_sizes() {
    for size in [(1100.0, 720.0), (760.0, 560.0), (640.0, 420.0)] {
        for index in 0..5 {
            let rt = runtime(fixtures::dashboard(index), size);
            for key in ["titulo", "nota", "estado", "vida", "situacao", "atalhos"] {
                let Some(([x, _, w, _], _)) = rt.presented(key) else {
                    continue;
                };
                assert!(
                    x >= 0.0 && x + w <= size.0 + 0.5,
                    "{key} of disk {index} spills at {size:?}: x={x} w={w}"
                );
            }
        }
    }
}

#[test]
fn fixtures_cover_the_four_states() {
    let disks = fixtures::disks();
    let cards: Vec<_> = disks
        .iter()
        .map(|snapshot| crate::present::Card::new(snapshot, NOW))
        .collect();
    assert_eq!(
        cards.iter().map(|card| card.tone()).collect::<Vec<_>>(),
        [
            Some(Tone::Healthy),
            Some(Tone::Attention),
            Some(Tone::Unknown),
            None,
            Some(Tone::Unknown)
        ]
    );
}
