//! The title bar the window draws itself (rune `App::titlebar`): the wordmark,
//! the export action and its confirmation. On Windows rune takes the native bar
//! away, keeps resizing, snap layouts, the window menu and the animations, and
//! draws the caption buttons over the trailing edge, so the row keeps clear of
//! them with `Cx::chrome_insets`. Everything not pressable moves the window.

use super::{
    BODY, CAPTION_STRONG, Dashboard, Palette, S1, S2, S3, S4, S5, Toast, export, export::Outcome,
};
use rune::{Cursor, El, Icon, Transition, Tx, el, icon, row, text, tok, tok::TextStyle};

/// A taller band than the system's 32 pt, so the wordmark has room.
pub const HEIGHT: f32 = 52.0;

/// The one bold type of the window: tight tracking, heavy weight.
const WORDMARK: TextStyle = TextStyle::new(22.0, 800.0).lh(28.0).tracking(-0.6);

static EXPORT: Icon = Icon {
    name: "chkdsk-export",
    view: 16.0,
    body: r#"<path d="M8 2.5v7.5M5 7.5l3 3 3-3M3 12.5h10" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

static CHECK: Icon = Icon {
    name: "chkdsk-check",
    view: 16.0,
    body: r#"<path d="M3.5 8.5l3 3 6-6.5" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

static WARNING: Icon = Icon {
    name: "chkdsk-warning",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" stroke-width="1.5"/><path d="M8 5v3.5" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/><circle cx="8" cy="11" r="0.9" fill="currentColor"/>"#,
};

/// `leading` and `trailing` are what the window's own controls take at each end.
pub fn bar(state: &Dashboard, palette: &Palette, leading: f32, trailing: f32) -> El {
    let t = palette.theme;
    // A window without the focus dims its name, as a system title bar does.
    let (ink, mark) = if state.focused {
        (t.text, palette.accent)
    } else {
        (t.text_secondary, palette.accent.alpha(0.5))
    };
    let wordmark = row()
        .key("marca")
        .center_y()
        .gap(S1)
        .child(
            text("chkdsk")
                .font(WORDMARK)
                .color(ink)
                .help_when_truncated("chkdsk"),
        )
        // The cursor of a prompt, in the accent: the same bar that marks the open disk.
        .child(el().w(3.0).h(S5).radius(1.5).fill(mark).mt(S1));
    row()
        .key("titlebar")
        .w_full()
        .h(HEIGHT)
        .center_y()
        .gap(S3)
        .pl(S5 + leading)
        .pr(S3 + trailing)
        .window_drag()
        .child(wordmark)
        .child(el().grow(1.0).min_w(0.0))
        .children(
            state
                .toast
                .as_ref()
                .map(|toast| confirmation(toast, palette)),
        )
        .child(export_button(state, palette))
}

fn export_button(state: &Dashboard, palette: &Palette) -> El {
    let t = palette.theme;
    let enabled = state.can_export();
    let ink = if state.focused {
        t.text
    } else {
        t.text_secondary
    };
    let help = if !export::SUPPORTED {
        "Exportar só está disponível no Windows"
    } else if enabled {
        "Exportar os discos para JSON ou CSV (Ctrl+E)"
    } else {
        "Não há discos para exportar"
    };
    let button = row()
        .key("exportar")
        .role("button")
        .label("Exportar discos")
        .help(help)
        .h(32.0)
        .px(S3)
        .gap(S2)
        .center_y()
        .radius(tok::radius::SM)
        .border(1.0, t.control_off)
        .fill(t.card)
        .child(icon(&EXPORT, S4).color(ink))
        .child(text("Exportar").font(BODY).color(ink));
    if enabled {
        button
            .hover(t.control_track)
            .press(t.control_off)
            .cursor(Cursor::Pointer)
            .on_press_tx(|s: &mut Dashboard, tx: &mut Tx| s.start_export(tx))
    } else {
        button.disabled(true).opacity(0.5)
    }
}

/// The result of the last export, beside the button: a check and the file name,
/// or why nothing was written.
fn confirmation(toast: &Toast, palette: &Palette) -> El {
    let (glyph, color, message) = match &toast.outcome {
        Outcome::Saved { name } => (&CHECK, palette.healthy, format!("Exportado: {name}")),
        Outcome::Unavailable => (
            &WARNING,
            palette.attention,
            "Exportar só está disponível no Windows".to_owned(),
        ),
        Outcome::Failed | Outcome::Cancelled => (
            &WARNING,
            palette.attention,
            "Não foi possível salvar o arquivo".to_owned(),
        ),
    };
    row()
        .key("confirmacao")
        .role("status")
        .label(message.clone())
        .min_w(0.0)
        .px(S3)
        .py(S1)
        .gap(S2)
        .center_y()
        .radius(S3)
        .fill(color.alpha(super::PILL_TINT))
        .child(icon(glyph, S4).color(color))
        .child(
            text(message)
                .font(CAPTION_STRONG)
                .color(color)
                .truncate()
                .min_w(0.0),
        )
        .transition(Transition::offset(S4, 0.0).combined(Transition::opacity()))
}
