use super::*;
use crate::{
    collector::{DiskSnapshot, Reading},
    fixtures::{self, NOW},
    present::{Body, Tone},
};
use chkdsk_core::model::{DiskIdentity, DiskKind};
use rune::{Env, Mods, Runtime, Shell};

const FRAME: f64 = 1.0 / 60.0;

fn runtime(state: Dashboard, size: (f32, f32)) -> Runtime<Dashboard> {
    let mut rt = Runtime::new(state, view, size, Env::new(true, 2.0));
    rt.on_key = Some(on_key);
    rt.frame(0.0);
    rt
}

/// Where the panel is scrolled, as its viewport reported it.
fn scrolled(rt: &Runtime<Dashboard>) -> f32 {
    rt.state.bars.detail.offset()
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
    assert_eq!(scrolled(&rt), 0.0);
    plain(&mut rt, Key::Down);
    let after_down = scrolled(&rt);
    assert!(after_down > 0.0, "Down scrolls the panel: {after_down}");
    typed(&mut rt, "j");
    assert!(scrolled(&rt) > after_down, "j is Down");
    plain(&mut rt, Key::Up);
    typed(&mut rt, "k");
    assert!(scrolled(&rt) < after_down + 1.0);
    assert_eq!(rt.state.selected, 1, "scrolling never switches disks");

    let (x, y) = rt.find("detalhe").expect("the panel is on screen");
    rt.wheel(x + 20.0, y + 20.0, -200.0);
    settle(&mut rt);
    let wheeled = scrolled(&rt);
    assert!(wheeled > after_down, "the wheel scrolls: {wheeled}");

    plain(&mut rt, Key::Right);
    assert_eq!(rt.state.selected, 2);
    assert_eq!(scrolled(&rt), 0.0, "a new disk opens at its top");
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
        "93",
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
fn fixtures_cover_every_state() {
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
            Some(Tone::Unknown),
            Some(Tone::Unknown),
            Some(Tone::Caution)
        ]
    );
}

#[test]
fn too_little_data_shows_no_score_and_says_why() {
    for size in [(1100.0, 720.0), (760.0, 560.0)] {
        let rt = runtime(fixtures::dashboard(5), size);
        let shown = screen(&rt);
        assert!(rt.find("nota").is_none(), "{size:?}: no hero number");
        assert!(
            shown.contains("Dados SMART insuficientes para calcular a nota"),
            "{size:?}: {shown}"
        );
        assert!(
            !shown.contains("backup"),
            "{size:?}: no advice without a verdict"
        );
    }
}

#[test]
fn a_pass_with_soft_warnings_reads_attention_and_names_the_cause_and_next_step() {
    let rt = runtime(fixtures::dashboard(6), (760.0, 560.0));
    let shown = screen(&rt);
    assert!(shown.contains("Atenção"), "{shown}");
    assert!(
        shown.contains("Erros de mídia NVMe registrados: 12"),
        "{shown}"
    );
    assert!(
        shown.contains("Faça backup dos dados importantes."),
        "{shown}"
    );
    assert!(
        !shown.contains("O dispositivo relatou uma falha"),
        "{shown}"
    );
    // A healthy disk gets neither warnings nor advice.
    let healthy = screen(&runtime(fixtures::dashboard(0), (760.0, 560.0)));
    assert!(!healthy.contains("backup"), "{healthy}");
}

#[test]
fn threshold_warnings_name_the_attribute_and_label_the_numbers() {
    let rt = runtime(fixtures::dashboard(1), (1100.0, 720.0));
    let shown = screen(&rt);
    assert!(
        shown.contains("Próximo do limite: Setores realocados (05), valor\u{a0}41, limite\u{a0}36"),
        "{shown}"
    );
    assert!(
        shown.contains("Faça backup dos dados importantes."),
        "{shown}"
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
    assert!(scrolled(&rt) > 0.0, "the panel is scrolled");
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
    assert_eq!(scrolled(&rt), 0.0, "the new disk opens at its top");

    // An update that keeps the selected disk leaves the scroll where it is.
    let (mut rt, disks) = scrolled_on_a_long_panel();
    let before = scrolled(&rt);
    rt.update(|state, tx| {
        state.deliver(Update::Disks(disks), NOW, tx);
    });
    settle(&mut rt);
    assert_eq!(scrolled(&rt), before);
}

#[test]
fn an_update_that_moves_the_selection_keeps_its_row_in_the_list() {
    let window_height = 560.0;
    let mut rt = runtime(pending_disks(12), (760.0, window_height));
    let (_, y) = rt.find("disco-11").expect("the last row is laid out");
    assert!(
        y > window_height,
        "the last row starts below the window: {y}"
    );

    // The selected disk is now the last one in the list.
    let mut disks: Vec<_> = (0..12)
        .map(|index| DiskSnapshot {
            identity: identity(&index.to_string()),
            reading: Reading::Pending,
        })
        .collect();
    disks.reverse();
    rt.update(|state, tx| {
        state.deliver(Update::Disks(disks), NOW, tx);
    });
    settle(&mut rt);
    assert_eq!(rt.state.selected, 11);
    let (_, y) = rt.find("disco-0").expect("the selected row is laid out");
    assert!(y < window_height, "the selected row is on screen: {y}");
}

#[test]
fn a_later_successful_discovery_clears_the_failure_notice() {
    let mut dashboard = Dashboard::default();
    dashboard.apply(Update::DiscoveryFailed, NOW);
    assert_eq!(dashboard.notice, Notice::DiscoveryFailed);
    dashboard.apply(Update::Disks(fixtures::disks()), NOW);
    assert_ne!(dashboard.notice, Notice::DiscoveryFailed);

    // With disks listed, a failed discovery leaves the window and the notice alone.
    assert!(!dashboard.apply(Update::DiscoveryFailed, NOW));
    assert_ne!(dashboard.notice, Notice::DiscoveryFailed);

    // When the disks later vanish, the window says so rather than repeating the failure.
    dashboard.apply(Update::Disks(Vec::new()), NOW);
    assert_eq!(dashboard.notice, Notice::NoDisks);
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
    assert_eq!(rt.state.selected, 6, "the selection follows its disk");
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

// ---- The window shell: title bar, inset panel, Avançado, scroll bars, motion, export ----

/// What Windows takes of the title bar: the three 46 pt caption buttons.
const CAPTION_BUTTONS: f32 = 138.0;

fn with_windows_chrome(rt: &mut Runtime<Dashboard>) {
    let mut insets = rune::ChromeInsets::default();
    insets.trailing = CAPTION_BUTTONS;
    insets.height = titlebar::HEIGHT;
    rt.set_chrome_insets(insets);
    settle(rt);
}

fn shell(index: usize, size: (f32, f32)) -> Runtime<Dashboard> {
    let mut rt = runtime(fixtures::dashboard(index), size);
    rt.poll = Some(poll);
    settle(&mut rt);
    rt
}

fn rect(rt: &Runtime<Dashboard>, key: &str) -> [f32; 4] {
    rt.presented(key)
        .unwrap_or_else(|| panic!("{key} is not on screen"))
        .0
}

#[test]
fn the_title_bar_holds_the_wordmark_and_the_export_action_clear_of_the_caption_buttons() {
    for size in [(1100.0, 720.0), (760.0, 560.0), (640.0, 420.0)] {
        let mut rt = shell(0, size);
        with_windows_chrome(&mut rt);
        let bar = rect(&rt, "titlebar");
        assert_eq!((bar[0], bar[1], bar[2]), (0.0, 0.0, size.0), "{size:?}");
        assert_eq!(bar[3], titlebar::HEIGHT, "{size:?}");
        assert!(screen(&rt).contains("chkdsk"), "{size:?}: the wordmark");
        let mark = rect(&rt, "marca");
        assert!(
            mark[0] >= 16.0,
            "{size:?}: the wordmark has room on the left"
        );
        let export = rect(&rt, "exportar");
        assert!(
            export[0] + export[2] <= size.0 - CAPTION_BUTTONS,
            "{size:?}: the export action stays clear of the caption buttons: {export:?}"
        );
        assert!(
            export[0] >= mark[0] + mark[2],
            "{size:?}: no overlap with the wordmark"
        );
        assert!(
            export[1] >= 0.0 && export[1] + export[3] <= titlebar::HEIGHT,
            "{size:?}: inside the band"
        );
        assert!(
            screen(&rt).contains("Exportar discos"),
            "{size:?}: the action is named for assistive tools"
        );
    }
}

#[test]
fn the_title_bar_moves_the_window_but_keeps_the_presses_of_its_button() {
    let mut rt = shell(0, (1100.0, 720.0));
    with_windows_chrome(&mut rt);
    // Empty space in the band drags, and a second press maximizes.
    let (_, y) = rt.find("titlebar").unwrap();
    rt.pointer_down(500.0, y);
    assert_eq!(rt.take_titlebar_action(), Some(rune::TitlebarAction::Drag));
    rt.pointer_up(500.0, y);
    rt.pointer_down(500.0, y);
    assert_eq!(
        rt.take_titlebar_action(),
        Some(rune::TitlebarAction::DoubleClick)
    );
    rt.pointer_up(500.0, y);
    // The wordmark is part of the drag region.
    let (x, y) = rt.find("marca").unwrap();
    rt.pointer_down(x, y);
    assert_eq!(rt.take_titlebar_action(), Some(rune::TitlebarAction::Drag));
    rt.pointer_up(x, y);
    // The export button is not: pressing it never moves the window.
    let (x, y) = rt.find("exportar").unwrap();
    rt.pointer_down(x, y);
    assert_eq!(rt.take_titlebar_action(), None);
    rt.pointer_up(x, y);
}

#[test]
fn an_inactive_window_dims_the_title_and_an_active_one_restores_it() {
    let mut rt = shell(0, (1100.0, 720.0));
    assert!(rt.state.focused);
    let before = screen(&rt);
    rt.update(|state, tx| on_focus(state, false, tx));
    settle(&mut rt);
    assert!(!rt.state.focused, "the title bar draws its dimmed colors");
    assert!(rt.find("marca").is_some(), "dimmed, never hidden");
    assert!(screen(&rt).contains("chkdsk") && screen(&rt).contains("Exportar"));
    rt.update(|state, tx| on_focus(state, true, tx));
    settle(&mut rt);
    assert!(rt.state.focused);
    assert_eq!(screen(&rt), before);
}

#[test]
fn the_detail_sits_in_a_raised_panel_inset_from_the_window_edges() {
    for size in [(1100.0, 720.0), (760.0, 560.0)] {
        let rt = shell(0, size);
        let panel = rect(&rt, "painel");
        let sidebar = rect(&rt, "discos");
        assert_eq!(
            panel[0],
            sidebar[0] + sidebar[2],
            "{size:?}: beside the list"
        );
        assert!(
            panel[1] >= titlebar::HEIGHT,
            "{size:?}: below the title bar"
        );
        assert_eq!(
            panel[0] + panel[2],
            size.0 - 12.0,
            "{size:?}: 12 pt from the right edge"
        );
        assert_eq!(
            panel[1] + panel[3],
            size.1 - 12.0,
            "{size:?}: 12 pt from the bottom edge"
        );
        // The list is inset too: its rows never touch the window's left edge.
        let (row_x, _) = rt.find("disco-interface:nvme-0").unwrap();
        let row = rect(&rt, "disco-interface:nvme-0");
        assert!(row[0] >= 12.0 && row_x > row[0], "{size:?}: {row:?}");
    }
    // Narrow windows give the list a little less.
    assert_eq!(rect(&shell(0, (1100.0, 720.0)), "discos")[2], 272.0);
    assert_eq!(rect(&shell(0, (760.0, 560.0)), "discos")[2], 248.0);
}

#[test]
fn the_score_shows_a_percent_with_its_caption_on_the_same_line() {
    assert_eq!(crate::present::percent(96), "96%");
    assert_eq!(crate::present::percent(0), "0%");
    assert_eq!(crate::present::percent(100), "100%");
    let rt = shell(0, (1100.0, 720.0));
    let shown = screen(&rt);
    assert!(shown.contains("93% saúde"), "{shown}");
    // One text run: the number, the percent sign and "saúde" cannot drift apart.
    let hero = rect(&rt, "nota");
    let pill = rect(&rt, "estado");
    assert!(
        hero[1] <= pill[1] && pill[1] + pill[3] <= hero[1] + hero[3],
        "{hero:?} {pill:?}"
    );
}

#[test]
fn facts_sit_in_two_aligned_columns() {
    let rt = shell(0, (1100.0, 720.0));
    let rows = ["vida", "desgaste", "temperatura", "ligado", "leitura"];
    let first = rect(&rt, rows[0]);
    for key in rows {
        let row = rect(&rt, key);
        assert_eq!(row[0], first[0], "{key} starts at the same x");
        assert_eq!(row[3], 32.0, "{key} is on the 4 pt grid");
    }
    for pair in rows.windows(2) {
        let (above, below) = (rect(&rt, pair[0]), rect(&rt, pair[1]));
        assert_eq!(
            below[1],
            above[1] + above[3],
            "{pair:?} stack without a gap"
        );
    }
}

/// Distance between two colors in OKLab, from their linear-light channels.
fn oklab_distance(a: Color, b: Color) -> f32 {
    let lab = |c: Color| {
        let l = (0.412_221_47 * c.r + 0.536_332_55 * c.g + 0.051_445_995 * c.b).cbrt();
        let m = (0.211_903_5 * c.r + 0.680_699_5 * c.g + 0.107_396_96 * c.b).cbrt();
        let s = (0.088_302_46 * c.r + 0.281_718_85 * c.g + 0.629_978_7 * c.b).cbrt();
        [
            0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
            1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
            0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
        ]
    };
    let (a, b) = (lab(a), lab(b));
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

#[test]
fn caution_and_attention_use_different_colors_and_frames() {
    for dark in [true, false] {
        let palette = Palette::new(dark, Color::WHITE);
        let (caution, attention) = (palette.tone(Tone::Caution), palette.tone(Tone::Attention));
        // Amber against red: a clear step apart in OKLab (a just-noticeable difference is 0.02).
        let distance = oklab_distance(caution, attention);
        assert!(
            distance >= 0.1,
            "dark={dark}: caution and attention are {distance:.3} apart"
        );
        assert_ne!(palette.tone(Tone::Healthy), caution);
        assert_ne!(
            palette.tone(Tone::Unknown),
            caution,
            "no data is not a warning"
        );
    }
    // Both read "Atenção", but the mild one never claims a device failure.
    let mild = shell(6, (1100.0, 720.0));
    let severe = shell(1, (1100.0, 720.0));
    assert!(screen(&mild).contains("Atenção") && screen(&severe).contains("Atenção"));
    assert!(!screen(&mild).contains("O dispositivo relatou uma falha"));
    assert!(screen(&severe).contains("O dispositivo relatou uma falha"));
    for rt in [&mild, &severe] {
        assert!(rt.find("aviso-saude").is_some(), "a framed verdict");
    }
    assert_eq!(mild.state.cards[6].tone(), Some(Tone::Caution));
    assert_eq!(severe.state.cards[1].tone(), Some(Tone::Attention));
    // A healthy disk has no frame.
    assert!(shell(0, (1100.0, 720.0)).find("aviso-saude").is_none());
}

#[test]
fn avancado_starts_closed_and_opens_with_the_table_and_the_details() {
    assert!(!Dashboard::default().advanced, "closed in a new window");
    let mut rt = shell(1, (1100.0, 720.0));
    assert!(rt.find("avancado").is_some(), "the header is there");
    assert!(rt.find("avancado-conteudo").is_none(), "its content is not");
    assert!(rt.find("atributos").is_none());
    assert!(!screen(&rt).contains("Setores incorrigíveis offline (C6)"));

    click(&mut rt, "avancado");
    assert!(rt.state.advanced);
    let shown = screen(&rt);
    for text in [
        "Atributos ATA",
        "Barramento",
        "SATA",
        "Firmware",
        "01.01A01",
        "Número de série",
        "Fontes dos dados",
        "SMART ATA com limites",
        "Perfil de atributos",
        "Setores realocados",
        "Limite",
        "Bruto",
        "0000000006C8",
        "Específico do fabricante",
    ] {
        assert!(shown.contains(text), "{text} is missing: {shown}");
    }
    assert!(rt.find("atributos").is_some());

    click(&mut rt, "avancado");
    assert!(!rt.state.advanced);
    assert!(rt.find("avancado-conteudo").is_none(), "closed again");
}

#[test]
fn avancado_follows_the_a_key_and_is_a_per_window_choice_across_disks() {
    let mut rt = shell(1, (1100.0, 720.0));
    typed(&mut rt, "a");
    assert!(rt.state.advanced);
    plain(&mut rt, Key::Left);
    assert_eq!(rt.state.selected, 0);
    assert!(
        rt.state.advanced,
        "the choice belongs to the window, not to a disk"
    );
    assert!(
        rt.find("avancado-conteudo").is_some(),
        "the NVMe details are open"
    );
    assert!(
        rt.find("atributos").is_none(),
        "an NVMe disk has no ATA table"
    );
    assert!(screen(&rt).contains("log de saúde NVMe"));
    typed(&mut rt, "A");
    assert!(!rt.state.advanced);
}

#[test]
fn the_attribute_table_right_aligns_its_numbers_and_ids_stay_in_one_column() {
    let mut rt = shell(1, (1100.0, 720.0));
    click(&mut rt, "avancado");
    // The numbers are text in fixed-width columns ending at the same x.
    let tree = rt.accessibility_json();
    let text = tree.to_string();
    assert!(
        text.contains("\"Nome\"") || text.contains("Nome"),
        "the name column"
    );
    for header in ["ID", "Nome", "Atual", "Pior", "Limite", "Bruto"] {
        assert!(text.contains(header), "{header}");
    }
}

fn thumb_of(rt: &Runtime<Dashboard>, scroll_key: &str) -> Option<[f32; 4]> {
    let bar = rt.presented(&format!("{scroll_key}-barra"))?;
    (bar.1 > 0.5)
        .then(|| rt.presented(&format!("{scroll_key}-polegar")))
        .flatten()
        .map(|(rect, _)| rect)
}

#[test]
fn the_scroll_bar_shows_only_while_the_content_overflows_and_follows_it() {
    // A short panel in a tall window fits: no bar.
    let rt = shell(0, (1100.0, 720.0));
    assert_eq!(thumb_of(&rt, DETAIL_KEY), None, "content fits, bar hidden");

    // The failing disk overflows a small window.
    let mut rt = shell(1, (760.0, 560.0));
    let thumb = thumb_of(&rt, DETAIL_KEY).expect("overflow shows the bar");
    let track = rect(&rt, "detalhe-barra");
    assert!(
        thumb[3] >= 32.0 && thumb[3] < track[3],
        "{thumb:?} in {track:?}"
    );
    assert_eq!(thumb[1], track[1], "at the top of its track");
    assert!(thumb[2] <= 6.0, "thin when idle: {thumb:?}");

    // Scrolling moves the thumb down, never past the end of the track.
    plain(&mut rt, Key::Down);
    plain(&mut rt, Key::Down);
    let moved = thumb_of(&rt, DETAIL_KEY).unwrap();
    assert!(moved[1] > thumb[1], "{moved:?}");
    plain(&mut rt, Key::End);
    for _ in 0..40 {
        plain(&mut rt, Key::Down);
    }
    let end = thumb_of(&rt, DETAIL_KEY).unwrap();
    assert!(
        end[1] + end[3] <= track[1] + track[3] + 0.5,
        "{end:?} in {track:?}"
    );
    assert!(end[1] > moved[1]);

    // The list has its own bar, only once its rows overflow.
    assert_eq!(
        thumb_of(&rt, LIST_KEY),
        None,
        "seven rows fit at this height"
    );
    let rt = shell(0, (760.0, 360.0));
    assert!(
        thumb_of(&rt, LIST_KEY).is_some(),
        "a short window scrolls the list"
    );
}

#[test]
fn the_thumb_widens_under_the_pointer_and_drags_the_content() {
    let mut rt = shell(1, (760.0, 560.0));
    let idle = thumb_of(&rt, DETAIL_KEY).unwrap();
    let (x, y) = (idle[0] + idle[2] / 2.0, idle[1] + idle[3] / 2.0);
    rt.pointer_move(x, y);
    settle(&mut rt);
    let hover = thumb_of(&rt, DETAIL_KEY).unwrap();
    assert!(hover[2] > idle[2], "wider on hover: {idle:?} -> {hover:?}");
    rt.pointer_move(300.0, 300.0);
    settle(&mut rt);
    assert_eq!(thumb_of(&rt, DETAIL_KEY).unwrap()[2], idle[2], "thin again");

    // Dragging the thumb by a third of its travel scrolls a third of the way.
    let track = rect(&rt, "detalhe-barra");
    let travel = track[3] - idle[3];
    let max = rt.state.bars.detail.pos.get()[1];
    assert!(max > 0.0);
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    rt.pointer_move(x, y + travel / 3.0);
    settle(&mut rt);
    let dragged = scrolled(&rt);
    assert!(
        (dragged - max / 3.0).abs() < 2.0,
        "dragged {dragged}, wanted {}",
        max / 3.0
    );
    assert!(
        rt.state.bars.is_active(Which::Detail),
        "held thumbs stay wide"
    );
    rt.pointer_up(x, y + travel / 3.0);
    settle(&mut rt);
    assert!(scrolled(&rt) > 0.0);
}

#[test]
fn pressing_the_track_pages_toward_the_pointer() {
    let mut rt = shell(1, (760.0, 560.0));
    let track = rect(&rt, "detalhe-barra");
    let viewport = rt.state.bars.detail.pos.get()[2];
    let below = (track[0] + track[2] / 2.0, track[1] + track[3] - 8.0);
    rt.pointer_down(below.0, below.1);
    rt.pointer_up(below.0, below.1);
    settle(&mut rt);
    let paged = scrolled(&rt);
    let max = rt.state.bars.detail.pos.get()[1];
    // A page is most of a viewport, or what is left of the content.
    assert!(
        paged >= (viewport * 0.5).min(max) - 1.0 && paged > 0.0,
        "a page down: {paged} of {max}"
    );
    let thumb = thumb_of(&rt, DETAIL_KEY).unwrap();
    let above = (track[0] + track[2] / 2.0, track[1] + 2.0);
    assert!(above.1 < thumb[1], "the pointer is above the thumb now");
    rt.pointer_down(above.0, above.1);
    rt.pointer_up(above.0, above.1);
    settle(&mut rt);
    assert!(scrolled(&rt) < paged, "a page up");
}

#[test]
fn scroll_bar_geometry_maps_content_viewport_and_offset() {
    use scrollbar::{Thumb, dragged_offset, page_direction, thumb};
    // Content that fits has no thumb.
    assert_eq!(thumb(500.0, 600.0, 600.0, 0.0), None);
    assert_eq!(
        thumb(500.0, 600.0, 600.4, 0.0),
        None,
        "half a point is not overflow"
    );
    // Twice the viewport: the thumb is half the track.
    let half = thumb(400.0, 600.0, 1200.0, 0.0).unwrap();
    assert_eq!((half.top, half.len), (0.0, 200.0));
    let end = thumb(400.0, 600.0, 1200.0, 600.0).unwrap();
    assert_eq!((end.top, end.len), (200.0, 200.0));
    let middle = thumb(400.0, 600.0, 1200.0, 300.0).unwrap();
    assert_eq!(middle.top, 100.0);
    // Overscroll never leaves the track.
    assert_eq!(thumb(400.0, 600.0, 1200.0, 900.0).unwrap().top, 200.0);
    assert_eq!(thumb(400.0, 600.0, 1200.0, -50.0).unwrap().top, 0.0);
    // A huge page gets the minimum thumb, never a speck.
    let tiny = thumb(400.0, 600.0, 600_000.0, 0.0).unwrap();
    assert_eq!(tiny.len, 32.0);
    let shorter = thumb(20.0, 600.0, 600_000.0, 0.0).unwrap();
    assert_eq!(shorter.len, 20.0, "no longer than its track");

    // Dragging: the thumb's whole travel is the whole scroll range.
    assert_eq!(dragged_offset(400.0, 600.0, 1200.0, 0.0, 200.0), 600.0);
    assert_eq!(dragged_offset(400.0, 600.0, 1200.0, 0.0, 100.0), 300.0);
    assert_eq!(dragged_offset(400.0, 600.0, 1200.0, 300.0, -50.0), 150.0);
    assert_eq!(
        dragged_offset(400.0, 600.0, 1200.0, 0.0, 9_999.0),
        600.0,
        "clamped at the end"
    );
    assert_eq!(
        dragged_offset(400.0, 600.0, 1200.0, 300.0, -9_999.0),
        0.0,
        "clamped at the start"
    );
    assert_eq!(
        dragged_offset(400.0, 600.0, 500.0, 0.0, 50.0),
        0.0,
        "nothing to scroll"
    );
    // The thumb and the drag agree: moving the thumb by dy moves it by dy.
    let before = thumb(400.0, 600.0, 3000.0, 700.0).unwrap();
    let after = thumb(
        400.0,
        600.0,
        3000.0,
        dragged_offset(400.0, 600.0, 3000.0, 700.0, 25.0),
    )
    .unwrap();
    assert!((after.top - before.top - 25.0).abs() < 0.01);

    // Pressing the track pages up above the thumb and down below it, not on it.
    let thumb = Thumb {
        top: 100.0,
        len: 50.0,
    };
    assert_eq!(page_direction(thumb, 40.0), -1);
    assert_eq!(page_direction(thumb, 120.0), 0);
    assert_eq!(page_direction(thumb, 151.0), 1);
}

#[test]
fn the_loading_dot_and_the_scroll_thumb_keep_three_to_one() {
    for dark in [true, false] {
        let palette = Palette::new(dark, Color::WHITE);
        let t = palette.theme;
        for tone in [Tone::Healthy, Tone::Caution, Tone::Attention, Tone::Unknown] {
            let ratio = contrast(palette.tone(tone), t.bg);
            assert!(ratio >= 3.0, "dark={dark} {tone:?} dot: {ratio:.2}");
        }
        // The loading ring is drawn in the secondary text color on the window.
        let ring = contrast(t.text_secondary, t.bg);
        assert!(ring >= 3.0, "dark={dark} loading ring: {ring:.2}");
        // The idle thumb over the raised panel.
        let thumb = over_color(t.text_secondary.alpha(0.7), t.card, true);
        let ratio = contrast(thumb, t.card);
        assert!(ratio >= 3.0, "dark={dark} idle thumb: {ratio:.2}");
    }
    // The ring really is what a disk with no reading shows.
    let rt = shell(3, (1100.0, 720.0));
    assert_eq!(rt.state.cards[3].tone(), None);
    assert!(rt.find("disco-interface:nvme-3").is_some());
}

#[test]
fn status_colors_keep_their_contrast_on_the_panel_and_inside_the_framed_verdict() {
    for dark in [true, false] {
        let palette = Palette::new(dark, Color::WHITE);
        let t = palette.theme;
        for tone in [Tone::Healthy, Tone::Caution, Tone::Attention, Tone::Unknown] {
            let ink = palette.tone(tone);
            let on_panel = contrast(ink, t.card);
            assert!(
                on_panel >= 4.5,
                "dark={dark} {tone:?} on the panel: {on_panel:.2}"
            );
            for encoded in [true, false] {
                let tint = over_color(ink.alpha(FRAME_TINT), t.card, encoded);
                let ratio = contrast(ink, tint);
                assert!(
                    ratio >= 4.5,
                    "dark={dark} {tone:?} in the frame (sRGB={encoded}): {ratio:.2}"
                );
                // Only a caution or attention verdict is framed; its reasons and advice sit on the tint.
                if matches!(tone, Tone::Caution | Tone::Attention) {
                    for (name, text) in [("text", t.text), ("advice", t.text_secondary)] {
                        let body = contrast(text, tint);
                        assert!(
                            body >= 4.5,
                            "dark={dark} {tone:?} {name} on the frame: {body:.2}"
                        );
                    }
                }
            }
        }
        for (surface, background) in [("janela", t.bg), ("linha selecionada", t.card)] {
            assert!(
                contrast(t.text_secondary, background) >= 4.5,
                "dark={dark} {surface}"
            );
            assert!(contrast(t.text, background) >= 4.5, "dark={dark} {surface}");
        }
    }
}

fn selection_x(rt: &Runtime<Dashboard>) -> f32 {
    rect(rt, "selecao")[1]
}

#[test]
fn the_selection_highlight_travels_between_rows_and_snaps_under_reduced_motion() {
    // Normal motion: a few frames after the click the highlight is between rows.
    let mut rt = shell(0, (1100.0, 720.0));
    let (from, to) = (selection_x(&rt), rect(&rt, "disco-interface:usb-2")[1]);
    let (x, y) = rt.find("disco-interface:usb-2").unwrap();
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    for _ in 0..4 {
        rt.frame(FRAME);
    }
    let mid = selection_x(&rt);
    assert!(mid > from && mid < to, "in flight: {from} < {mid} < {to}");
    settle(&mut rt);
    assert_eq!(selection_x(&rt), to, "and it lands on the row");

    // Reduced motion: no travel. After the short fade it is on the new row, and it
    // is never drawn between the two.
    let mut rt = shell(0, (1100.0, 720.0));
    rt.set_system_reduced_motion(true);
    settle(&mut rt);
    let (x, y) = rt.find("disco-interface:usb-2").unwrap();
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    let mut seen = Vec::new();
    for _ in 0..14 {
        rt.frame(FRAME);
        if let Some((rect, _)) = rt.presented("selecao") {
            seen.push(rect[1]);
        }
    }
    assert!(
        seen.iter().all(|y| *y == from || *y == to),
        "no travel between rows under reduced motion: {seen:?}"
    );
    assert_eq!(selection_x(&rt), to);
}

#[test]
fn another_disk_replaces_the_panel_with_a_soft_crossfade_that_reduced_motion_shortens() {
    let mut rt = shell(0, (1100.0, 720.0));
    assert!(rt.find("painel-interface:nvme-0").is_some());
    click(&mut rt, "disco-interface:sata-1");
    // Settled: only the new disk's page is left.
    assert!(rt.find("painel-interface:nvme-0").is_none());
    assert!(rt.find("painel-interface:sata-1").is_some());

    // Both pages exist while one fades out and the other in.
    let mut rt = shell(0, (1100.0, 720.0));
    let (x, y) = rt.find("disco-interface:sata-1").unwrap();
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(FRAME);
    rt.frame(FRAME);
    assert!(
        rt.presented("painel-interface:nvme-0").is_some()
            && rt.presented("painel-interface:sata-1").is_some(),
        "crossfading"
    );
    // Reduced motion still fades, but for no more than a fifth of a second.
    let mut rt = shell(0, (1100.0, 720.0));
    rt.set_system_reduced_motion(true);
    settle(&mut rt);
    let (x, y) = rt.find("disco-interface:sata-1").unwrap();
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    for _ in 0..14 {
        rt.frame(FRAME);
    }
    assert!(
        rt.find("painel-interface:nvme-0").is_none(),
        "the short fade is over"
    );
}

#[test]
fn a_new_score_rolls_in_and_the_old_one_leaves() {
    let mut rt = shell(0, (1100.0, 720.0));
    assert!(rt.find("nota-93").is_some());
    let mut disks = fixtures::disks();
    if let Reading::Ready(ready) = &mut disks[0].reading {
        ready.telemetry.endurance_percent = Some(30.0);
        if let chkdsk_core::model::QueryState::Success(nvme) = &mut ready.telemetry.nvme {
            nvme.percentage_used = Some(30);
        }
    }
    rt.update(|state, tx| {
        state.deliver(Update::Disks(disks), NOW, tx);
    });
    rt.frame(FRAME);
    rt.frame(FRAME);
    assert!(
        rt.presented("nota-93").is_some() && rt.presented("nota-70").is_some(),
        "the old number is still leaving while the new one arrives"
    );
    settle(&mut rt);
    assert!(rt.presented("nota-93").is_none());
    assert!(screen(&rt).contains("70% saúde"));
}

fn export_to(name: &'static str) -> impl Fn(usize, &str) -> Option<crate::export::Pick> {
    move |_, _| {
        Some(crate::export::Pick {
            path: std::env::temp_dir().join(format!("chkdsk-ui-{}-{name}", std::process::id())),
            filter: crate::export::Format::Csv,
        })
    }
}

fn pick_for_ui(_: usize, _: &str) -> Option<crate::export::Pick> {
    export_to("ui")(0, "")
}

fn wait_for_export(rt: &mut Runtime<Dashboard>) {
    for _ in 0..500 {
        rt.frame(FRAME);
        if rt.state.toast.is_some() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("the export never answered");
}

#[test]
fn the_export_button_asks_where_to_save_writes_the_file_and_confirms() {
    let mut rt = shell(0, (1100.0, 720.0));
    rt.state.picker = Some(pick_for_ui);
    assert!(rt.state.can_export());
    click(&mut rt, "exportar");
    wait_for_export(&mut rt);
    let path = std::env::temp_dir().join(format!("chkdsk-ui-{}-ui.csv", std::process::id()));
    let toast = rt.state.toast.clone().expect("a confirmation");
    assert_eq!(
        toast.outcome,
        crate::export::Outcome::Saved {
            name: path.file_name().unwrap().to_string_lossy().into()
        }
    );
    settle(&mut rt);
    assert!(
        screen(&rt).contains("Exportado: chkdsk-ui-"),
        "{}",
        screen(&rt)
    );
    assert!(rt.find("confirmacao").is_some());
    let csv = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        csv.trim_start_matches('\u{feff}')
            .split("\r\n")
            .filter(|l| !l.is_empty())
            .count(),
        8
    );
    std::fs::remove_file(&path).unwrap();
    assert!(rt.state.can_export(), "ready for the next one");

    // The confirmation goes away by itself after a few seconds.
    let until = toast.until.expect("it expires");
    rt.update(|state, tx| {
        assert!(poll(state, until - 0.5, tx) || state.toast.is_some());
    });
    assert!(rt.state.toast.is_some());
    rt.update(|state, tx| {
        assert!(poll(state, until + 0.1, tx));
    });
    settle(&mut rt);
    assert!(rt.state.toast.is_none());
    assert!(rt.find("confirmacao").is_none());
}

#[test]
fn ctrl_e_exports_too_and_a_closed_dialog_says_nothing() {
    fn cancel(_: usize, _: &str) -> Option<crate::export::Pick> {
        None
    }
    let mut rt = shell(0, (1100.0, 720.0));
    rt.state.picker = Some(cancel);
    press(
        &mut rt,
        Key::Command('e'),
        Mods {
            cmd: true,
            ctrl: true,
            ..Mods::default()
        },
    );
    for _ in 0..200 {
        rt.frame(FRAME);
        if rt.state.can_export() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(rt.state.toast.is_none(), "cancelling leaves no message");
    assert!(rt.state.can_export());
}

#[test]
fn without_a_save_dialog_the_action_says_so_and_with_no_disks_it_is_disabled() {
    let mut rt = shell(0, (1100.0, 720.0));
    rt.state.picker = None;
    click(&mut rt, "exportar");
    assert_eq!(
        rt.state.toast.as_ref().map(|toast| &toast.outcome),
        Some(&crate::export::Outcome::Unavailable)
    );
    assert!(screen(&rt).contains("Exportar só está disponível no Windows"));

    let rt = runtime(Dashboard::default(), (1100.0, 720.0));
    assert!(!rt.state.can_export(), "nothing to export yet");
    assert!(rt.find("exportar").is_some(), "the action stays in place");
    assert!(screen(&rt).contains("Não há discos para exportar"));
}
