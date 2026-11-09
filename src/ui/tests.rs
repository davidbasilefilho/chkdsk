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
    click(&mut rt, "disco-interface:usb-2");
    assert_eq!(rt.state.selected, 2);
    assert!(screen(&rt).contains("Crucial MX500 500GB"));
    click(&mut rt, "disco-interface:sata-1");
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
        (0, "Saudável", Some(Tone::Healthy)),
        (1, "Atenção", Some(Tone::Attention)),
        (2, "Indisponível", Some(Tone::Unknown)),
    ];
    for (index, label, tone) in cases {
        let dashboard = fixtures::dashboard(index);
        assert_eq!(dashboard.cards[index].tone(), tone);
        let rt = runtime(dashboard, (1100.0, 720.0));
        assert!(screen(&rt).contains(label), "{label} is shown");
    }
    for (index, text) in [
        (3, "Consultando o disco…"),
        (4, "Uma nova tentativa acontece em até 15 segundos"),
    ] {
        let rt = runtime(fixtures::dashboard(index), (1100.0, 720.0));
        assert!(screen(&rt).contains(text), "{text} is shown");
    }
}

#[test]
fn unknown_health_keeps_the_lifetime_estimate_visible() {
    // The denied disk has no usable telemetry, so no number, but its history still extrapolates.
    for size in [(1100.0, 720.0), (760.0, 560.0), (640.0, 420.0)] {
        let rt = runtime(fixtures::dashboard(2), size);
        let shown = screen(&rt);
        assert!(!shown.contains("N/A"), "{size:?}");
        assert!(rt.find("nota").is_none(), "{size:?}: no hero number");
        assert!(shown.contains("Indisponível"), "{size:?}");
        assert!(shown.contains("Vida estimada"), "{size:?}");
        assert!(shown.contains("1246 dias"), "{size:?}: {shown}");
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
        "saúde",
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
            for key in ["titulo", "nota", "estado", "vida", "situacao"] {
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

/// Two disks whose panels both overflow a small window: the second is open and scrolled.
fn scrolled_on_a_long_panel() -> (Runtime<Dashboard>, Vec<DiskSnapshot>) {
    let mut twin = fixtures::disks()[1].clone();
    twin.identity.id = "interface:sata-9".into();
    let mut disks = vec![twin];
    disks.push(fixtures::disks()[1].clone());
    let mut dashboard = Dashboard::default();
    dashboard.apply(Update::Disks(disks.clone()), NOW);
    dashboard.selected = 1;
    let mut rt = runtime(dashboard, (760.0, 560.0));
    plain(&mut rt, Key::Down);
    plain(&mut rt, Key::Down);
    assert!(rt.state.scroll.get() > 0.0, "the panel is scrolled");
    (rt, disks)
}

#[test]
fn a_selection_moved_by_an_update_opens_its_panel_at_the_top() {
    let (mut rt, mut disks) = scrolled_on_a_long_panel();
    disks.remove(1);
    rt.update(|state, tx| {
        state.deliver(Update::Disks(disks), NOW, tx);
    });
    settle(&mut rt);
    assert_eq!(rt.state.selected, 0, "the selected disk vanished");
    assert_eq!(rt.state.scroll.get(), 0.0, "the new disk opens at its top");

    // An update that keeps the selected disk leaves the scroll where it is.
    let (mut rt, disks) = scrolled_on_a_long_panel();
    let scrolled = rt.state.scroll.get();
    rt.update(|state, tx| {
        state.deliver(Update::Disks(disks), NOW, tx);
    });
    settle(&mut rt);
    assert_eq!(rt.state.scroll.get(), scrolled);
}

#[test]
fn disk_switching_ignores_ctrl_alt_and_super() {
    let mut rt = runtime(pending_disks(4), (1100.0, 720.0));
    plain(&mut rt, Key::Right);
    assert_eq!(rt.state.selected, 1);
    for mods in [
        Mods {
            ctrl: true,
            ..Mods::default()
        },
        Mods {
            alt: true,
            ..Mods::default()
        },
        Mods {
            logo: true,
            ..Mods::default()
        },
        Mods {
            cmd: true,
            ..Mods::default()
        },
    ] {
        for key in [Key::Left, Key::Right, Key::Tab] {
            press(&mut rt, key, mods);
            assert_eq!(rt.state.selected, 1, "{mods:?} must not switch disks");
        }
    }
    press(
        &mut rt,
        Key::Tab,
        Mods {
            shift: true,
            ..Mods::default()
        },
    );
    assert_eq!(rt.state.selected, 0, "Shift+Tab still goes back");
}

#[test]
fn list_rows_follow_the_disk_not_its_position() {
    let mut rt = runtime(fixtures::dashboard(0), (1100.0, 720.0));
    let mut disks = fixtures::disks();
    disks.reverse();
    rt.update(|state, tx| {
        state.deliver(Update::Disks(disks), NOW, tx);
    });
    settle(&mut rt);
    assert_eq!(rt.state.selected, 4, "the selection follows its disk");
    click(&mut rt, "disco-interface:usb-2");
    assert_eq!(rt.state.cards[rt.state.selected].id, "interface:usb-2");
}

#[test]
fn the_list_says_each_status_in_words_and_has_no_shortcut_footer() {
    let rt = runtime(fixtures::dashboard(0), (1100.0, 720.0));
    let shown = screen(&rt);
    for label in [
        "Saudável",
        "Atenção",
        "Indisponível",
        "Lendo…",
        "Samsung SSD 990 PRO 1TB, NVMe  ·  931,5 GiB, Saudável",
    ] {
        assert!(shown.contains(label), "{label} is missing: {shown}");
    }
    assert!(rt.find("atalhos").is_none());
    assert!(!shown.contains("q sair"));
}

#[test]
fn a_device_failure_verdict_is_shown_with_the_attention_color() {
    let mut rt = runtime(fixtures::dashboard(1), (1100.0, 720.0));
    settle(&mut rt);
    let tree = screen(&rt);
    assert!(tree.contains("O dispositivo relatou uma falha"));
    let Body::Health(health) = &rt.state.cards[1].body else {
        panic!("the failing disk has a reading");
    };
    assert_eq!(health.status_tone, Some(Tone::Attention));
}

fn srgb_encode(linear: f32) -> f32 {
    if linear <= 0.003_130_8 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

fn srgb_decode(encoded: f32) -> f32 {
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

type Curve = fn(f32) -> f32;

fn contrast(a: Color, b: Color) -> f32 {
    let (hi, lo) = (
        a.luminance().max(b.luminance()),
        a.luminance().min(b.luminance()),
    );
    (hi + 0.05) / (lo + 0.05)
}

/// Composite `top` over an opaque `bottom`, in sRGB-encoded space (as the window blends) or linear light.
fn over_color(top: Color, bottom: Color, encoded: bool) -> Color {
    let (to, from): (Curve, Curve) = if encoded {
        (srgb_encode, srgb_decode)
    } else {
        (|v| v, |v| v)
    };
    let mix = |t: f32, b: f32| from(to(t) * top.a + to(b) * (1.0 - top.a));
    Color {
        r: mix(top.r, bottom.r),
        g: mix(top.g, bottom.g),
        b: mix(top.b, bottom.b),
        a: 1.0,
    }
}

#[test]
fn status_text_keeps_four_and_a_half_to_one_in_both_themes() {
    for (dark, name) in [(false, "claro"), (true, "escuro")] {
        let palette = Palette::new(dark, Color::WHITE);
        for (tone, tone_name) in [
            (Tone::Healthy, "saudável"),
            (Tone::Attention, "atenção"),
            (Tone::Unknown, "indisponível"),
        ] {
            let ink = palette.tone(tone);
            // The pill: its text over its tint of the same color, over the window.
            for encoded in [true, false] {
                let pill = over_color(ink.alpha(PILL_TINT), palette.theme.bg, encoded);
                let ratio = contrast(ink, pill);
                eprintln!(
                    "pill {name} {tone_name} blend={}: {ratio:.2}:1",
                    if encoded { "sRGB" } else { "linear" }
                );
                assert!(ratio >= 4.5, "pill {name} {tone_name}: {ratio:.2}");
            }
            // The same color as plain text on the window and on the list.
            for (surface, background) in
                [("janela", palette.theme.bg), ("lista", palette.theme.card)]
            {
                let ratio = contrast(ink, background);
                eprintln!("text {name} {tone_name} on {surface}: {ratio:.2}:1");
                assert!(
                    ratio >= 4.5,
                    "text {name} {tone_name} on {surface}: {ratio:.2}"
                );
            }
        }
        // The status words and captions of the list.
        for (surface, background) in [
            ("lista", palette.theme.card),
            (
                "linha selecionada",
                over_color(palette.theme.control_track, palette.theme.card, true),
            ),
        ] {
            let ratio = contrast(palette.theme.text_secondary, background);
            eprintln!("secondary {name} on {surface}: {ratio:.2}:1");
            assert!(ratio >= 4.5, "secondary {name} on {surface}: {ratio:.2}");
        }
    }
}
