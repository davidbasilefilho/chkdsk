// Behavioral acceptance of the reconstructed interface, using real app input
// and renderer cells. Native Windows terminal/hardware acceptance is separate.

#[test]
fn panel_toggles_keep_their_hitboxes_for_repeated_mouse_clicks() {
    for width in [8, 12, 30, 49, 50, 70, 100, 160] {
        for height in [1, 2, 6, 10, 20, 28, 40] {
            for details in [true, false] {
                let state = redesigned_fixture();
                let initial = layout_for_state(width, height, &state);
                let button = if details { initial.detail_button } else { initial.notification_button };
                assert!(button.visible(), "missing toggle at {width}x{height}");
                for y in button.y..button.y + button.height {
                    for x in button.x..button.x + button.width {
                        let mut app = DashboardApp { started: true, ..Default::default() };
                        let mut global = test_global_data(state.clone(), width, height);
                        for opened in [true, false, true, false] {
                            dispatch_input(&mut app, &mut global, mouse_event(MouseInputKind::MouseDown(Button::Left), x, y));
                            assert_eq!(global.state.details_open, details && opened, "details at {width}x{height}, ({x},{y})");
                            assert_eq!(global.state.notifications_open, !details && opened, "notices at {width}x{height}, ({x},{y})");
                            let layout = layout_for_state(width, height, &global.state);
                            let active = if details { layout.detail_button } else { layout.notification_button };
                            assert_eq!((active.x, active.y, active.width, active.height), (button.x, button.y, button.width, button.height), "toggle moved at {width}x{height}");
                            if opened {
                                let other = if details { layout.notification_button } else { layout.detail_button };
                                assert!(!other.visible());
                            }
                        }
                    }
                }
            }
        }
    }
}

fn redesigned_fixture() -> Dashboard {
    let mut nvme = Telemetry::default();
    nvme.apply_nvme(crate::model::NvmeHealth { percentage_used: Some(12), ..Default::default() });
    Dashboard {
        disks: vec![DiskView {
            identity: DiskIdentity { model: Some("NVMe principal".into()), ..identity("nvme-main", 0) },
            telemetry: Some(nvme), querying: false, computing: false, pending_telemetry: None, error_message: None,
        }, DiskView {
            identity: DiskIdentity { model: Some("SATA secundário".into()), ..identity("sata-other", 1) },
            telemetry: None, querying: false, computing: false, pending_telemetry: None, error_message: None,
        }],
        ..Default::default()
    }
}

#[test]
fn reconstructed_main_keeps_only_ascii_percentage_eta_and_named_tabs() {
    let state = redesigned_fixture();
    let layout = layout_for_state(100, 28, &state);
    let mut app = DashboardApp { started: true, ..Default::default() };
    let mut global = test_global_data(state, 100, 28);
    let rendered = rendered_text(&mut app, &mut global);
    assert!(rendered.contains("NVMe principal") && rendered.contains("SATA secundário"), "{rendered}");
    let body = rendered.lines().skip(layout.body_y).collect::<Vec<_>>().join("\n");
    assert!(body.contains("88%") && body.contains("ETA —"), "{body}");
    assert!(body.lines().filter(|line| line.contains('#')).count() >= 5, "{body}");
    for forbidden in ["NVMe principal", "SAUDÁVEL", "Confiança", "CAPACIDADE", "desgaste", "indisponível", "DETALHES DO DISCO"] {
        assert!(!body.contains(forbidden), "main leaked {forbidden}: {body}");
    }
    assert!(!global.state.details_open && !global.state.notifications_open);
}

#[test]
fn panels_are_exclusive_hide_other_toggle_and_close_with_keyboard_and_mouse() {
    let mut app = DashboardApp { started: true, ..Default::default() };
    let mut global = test_global_data(redesigned_fixture(), 100, 28);
    dispatch_input(&mut app, &mut global, InputEvent::Keyboard(KeyPress::Plain { key: Key::Character('d') }));
    let layout = layout_for_state(100, 28, &global.state);
    assert!(global.state.details_open && !global.state.notifications_open);
    assert!(!layout.notification_button.visible() && layout.detail_button.visible());
    assert!(rendered_text(&mut app, &mut global).contains("DETALHES DO DISCO"));
    dispatch_input(&mut app, &mut global, InputEvent::Keyboard(KeyPress::Plain { key: Key::Character('n') }));
    let layout = layout_for_state(100, 28, &global.state);
    assert!(!global.state.details_open && global.state.notifications_open);
    assert!(!layout.detail_button.visible() && layout.notification_button.visible());
    dispatch_input(&mut app, &mut global, mouse_event(MouseInputKind::MouseDown(Button::Left), layout.notification_button.x + 1, layout.notification_button.y + 1));
    assert!(!global.state.notifications_open && !global.state.details_open);
    let layout = layout_for_state(100, 28, &global.state);
    dispatch_input(&mut app, &mut global, mouse_event(MouseInputKind::MouseDown(Button::Left), layout.detail_button.x + 1, layout.detail_button.y + 1));
    assert!(global.state.details_open);
    assert!(matches!(dispatch_input(&mut app, &mut global, InputEvent::Keyboard(KeyPress::Plain { key: Key::SpecialKey(SpecialKey::Esc) })), EventPropagation::ConsumedRender));
    assert!(!global.state.details_open);
}

#[test]
fn overlay_blank_cells_are_opaque_and_underlying_footer_cannot_receive_clicks() {
    for notifications_open in [false, true] {
        let state = Dashboard { details_open: !notifications_open, notifications_open, ..redesigned_fixture() };
        let layout = layout_for_state(70, 20, &state);
        assert_eq!(layout.panel.x, 0); // compact overlay covers the old footer
        let buffer = fixture_buffer(state.clone(), 70, 20);
        for y in layout.panel.y..layout.panel.y + layout.panel.height {
            for x in layout.panel.x..layout.panel.x + layout.panel.width {
                assert!(matches!(&buffer[y][x], r3bl_tui::PixelChar::PlainText { style, .. } if matches!(style.color_bg, Some(TuiColor::Rgb(c)) if c == TOKYO_BG_DARK1 || c == TOKYO_CYAN)), "unpainted/translucent cell at {x},{y}");
            }
        }
        let mut app = DashboardApp { started: true, ..Default::default() };
        let mut global = test_global_data(state, 70, 20);
        let original = global.state.selected;
        for (x, y) in [(1, 19), (45, 18), (34, 10)] {
            dispatch_input(&mut app, &mut global, mouse_event(MouseInputKind::MouseDown(Button::Left), x, y));
            assert_eq!(global.state.selected, original, "click-through at {x},{y}");
        }
    }
}

#[test]
fn details_scroll_mouse_and_keyboard_remain_independent_and_main_does_not_scroll() {
    let mut app = DashboardApp { started: true, ..Default::default() };
    let mut global = test_global_data(redesigned_fixture(), 100, 28);
    dispatch_input(&mut app, &mut global, InputEvent::Keyboard(KeyPress::Plain { key: Key::SpecialKey(SpecialKey::Down) }));
    assert_eq!(global.state.content_scroll, 0);
    dispatch_input(&mut app, &mut global, InputEvent::Keyboard(KeyPress::Plain { key: Key::Character('d') }));
    let layout = layout_for_state(100, 28, &global.state);
    dispatch_input(&mut app, &mut global, mouse_event(MouseInputKind::ScrollDown, layout.panel.x + 2, layout.panel.y + 4));
    assert_eq!(global.state.content_scroll, 1);
    dispatch_input(&mut app, &mut global, InputEvent::Keyboard(KeyPress::Plain { key: Key::SpecialKey(SpecialKey::Up) }));
    assert_eq!(global.state.content_scroll, 0);
    let lines = disk_detail_lines(&global.state.disks[0], &global.state.history, 43);
    assert!(lines.iter().any(|l| l.text.contains("Confiança")));
    assert!(lines.iter().any(|l| l.text.contains("requer histórico")));
    // Switching panels can't leave hidden detail scroll controls active.
    dispatch_input(&mut app, &mut global, InputEvent::Keyboard(KeyPress::Plain { key: Key::Character('n') }));
    assert!(!global.state.details_open);
}

#[test]
fn notification_collection_retires_resolved_errors_without_repeating_persistent_toasts() {
    let mut app = DashboardApp { started: true, ..Default::default() };
    let mut global = test_global_data(redesigned_fixture(), 100, 28);
    let identities = global.state.disks.iter().map(|d| d.identity.clone()).collect::<Vec<_>>();
    dispatch_signal(&mut app, &mut global, AppSignal::Discovered { generation: 0, result: Err("failed".into()) });
    assert_eq!(global.state.notifications.len(), 1);
    let first_generation = global.state.collection_generation;
    dispatch_signal(&mut app, &mut global, AppSignal::Discovered { generation: 0, result: Err("failed".into()) });
    assert_eq!(global.state.notifications.len(), 1);
    assert!(active_notification(&global.state, Instant::now()).is_none());
    assert!(global.state.collection_generation > first_generation);
    dispatch_signal(&mut app, &mut global, AppSignal::Discovered { generation: 0, result: Ok(identities.clone()) });
    assert!(global.state.notifications.is_empty());
    for _ in 0..5 {
        dispatch_signal(&mut app, &mut global, AppSignal::QueryFinished { generation: 0, disk_id: identities[0].id.clone(), result: Err(QueryErrorState::Permission) });
        assert_eq!(global.state.notifications.len(), 1);
    }
    dispatch_signal(&mut app, &mut global, AppSignal::Discovered { generation: 0, result: Ok(identities) });
    assert!(global.state.notifications.is_empty());
    assert!(global.state.disks.len() == 2, "partial/error drives remain visible");
}

#[test]
fn named_tab_buttons_keep_one_cell_padding_and_bold_centered_labels() {
    let state = redesigned_fixture();
    let layout = layout_for_state(100, 28, &state);
    let buffer = fixture_buffer(state, 100, 28);
    for tab in &layout.tabs {
        assert_eq!(tab.height, 3);
        for y in [tab.y, tab.y + tab.height - 1] {
            for x in tab.x..tab.x + tab.width {
                assert!(matches!(buffer[y][x], r3bl_tui::PixelChar::PlainText { display_char: ' ', .. }));
            }
        }
        for x in [tab.x, tab.x + tab.width - 1] {
            assert!(matches!(buffer[tab.y + 1][x], r3bl_tui::PixelChar::PlainText { display_char: ' ', .. }));
        }
        for pixel in buffer[tab.y + 1].iter().skip(tab.x + 1).take(tab.width - 2) {
            if let r3bl_tui::PixelChar::PlainText { style, display_char, .. } = pixel
                && *display_char != ' ' {
                assert!(style.attribs.bold.is_some());
            }
        }
    }
}

#[test]
fn reconstructed_renderer_exports_main_details_and_notifications_at_multiple_sizes() {
    let Some(output_dir) = std::env::var_os("CHKDSK_UI_PROOF_DIR").map(PathBuf::from) else { return; };
    let mut html = String::from("<!doctype html><meta charset=\"utf-8\"><title>Reconstructed chkdsk renderer</title><style>body{margin:24px;background:#222436;color:#c8d3f5;font:14px system-ui}.case{margin:20px 0 5px;color:#828bb8}.viewport{display:inline-block;overflow:auto}.row{display:grid;grid-auto-rows:16px}.cell{display:block;width:8px;height:16px;overflow:hidden;font:13px/16px monospace;white-space:pre}.cell.wide{grid-column:span 2;width:16px}</style>");
    let mut baseline = redesigned_fixture();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    for (offset, used) in [(86_400, 11.0), (0, 12.0)] {
        baseline.history.record(HealthSample { disk_id: "nvme-main".into(), timestamp_unix_secs: now-offset, kind: DiskKind::Nvme, endurance_percent: Some(used), power_on_hours: None, score: None }).unwrap();
    }
    push_notification(&mut baseline, Some("nvme-main".into()), "Aviso sintético".into(), vec!["Fixture; não é evidência de hardware físico.".into()], NotificationSeverity::Info, Instant::now());
    for (label, details_open, notifications_open) in [("Principal", false, false), ("Detalhes", true, false), ("Avisos", false, true)] {
        for (width, height) in [(100, 28), (70, 20), (30, 10), (12, 6)] {
            let state = Dashboard { details_open, notifications_open, ..baseline.clone() };
            let buffer = fixture_buffer(state, width, height);
            append_fixture_cells(&mut html, label, width, height, &buffer);
        }
    }
    std::fs::create_dir_all(&output_dir).unwrap();
    std::fs::write(output_dir.join("chkdsk-reconstructed-proof.html"), html).unwrap();
}

#[test]
fn compact_panels_reserve_readable_rows_before_showing_scroll_buttons() {
    for width in [12, 30, 70, 100] {
        for height in 11..=20 {
            let state = Dashboard { details_open: true, ..redesigned_fixture() };
            let layout = layout_for_state(width, height, &state);
            assert!(panel_visible_lines(&layout) >= 1, "panel content hidden at {width}x{height}: {layout:?}");
            for control in layout.controls.iter().filter(|c| layout.panel.contains(c.area.x, c.area.y)) {
                assert!(control.area.y >= layout.panel.y + 4);
                assert!(control.area.y + control.area.height <= height);
            }
        }
    }
}
