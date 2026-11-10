//! Scenes only: headless captures run on Linux, where rune draws no Windows caption
//! buttons, so `CHKDSK_SIM_CAPTION=1` fakes what Windows takes of the title bar
//! (three 46 pt buttons) and draws placeholders there. The title bar reserves the
//! simulated inset exactly as it reserves the real `Cx::chrome_insets`, so a capture
//! shows whether Exportar and its status keep clear of the buttons.

use super::{Palette, titlebar};
use rune::{ChromeInsets, El, Icon, el, icon, row};
use std::sync::OnceLock;

/// Windows' caption button width, and how many there are.
const BUTTON_WIDTH: f32 = 46.0;
const BUTTONS: usize = 3;

static MINIMIZE: Icon = Icon {
    name: "chkdsk-sim-minimize",
    view: 10.0,
    body: r#"<path d="M0 5h10" fill="none" stroke="currentColor" stroke-width="1"/>"#,
};

static MAXIMIZE: Icon = Icon {
    name: "chkdsk-sim-maximize",
    view: 10.0,
    body: r#"<rect x="0.5" y="0.5" width="9" height="9" rx="1.5" fill="none" stroke="currentColor" stroke-width="1"/>"#,
};

static CLOSE: Icon = Icon {
    name: "chkdsk-sim-close",
    view: 10.0,
    body: r#"<path d="M0 0l10 10M10 0L0 10" fill="none" stroke="currentColor" stroke-width="1"/>"#,
};

fn simulated() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CHKDSK_SIM_CAPTION").is_some_and(|value| value == "1"))
}

/// What Windows would take: the buttons at the trailing edge, the band's height.
fn simulated_insets(mut insets: ChromeInsets) -> ChromeInsets {
    insets.trailing = BUTTON_WIDTH * BUTTONS as f32;
    insets.height = titlebar::HEIGHT;
    insets
}

/// The insets the view lays the title bar out with.
pub fn insets(real: ChromeInsets) -> ChromeInsets {
    if simulated() {
        simulated_insets(real)
    } else {
        real
    }
}

/// Placeholder caption buttons over the trailing edge, or none.
pub fn caption_buttons(palette: &Palette) -> Option<El> {
    simulated().then(|| {
        let ink = palette.theme.text;
        let button = |glyph: &'static Icon| {
            el().w(BUTTON_WIDTH)
                .h(titlebar::HEIGHT)
                .center()
                .child(icon(glyph, 10.0).color(ink))
        };
        row()
            .key("legenda-simulada")
            .pin(None, Some(0.0), Some(0.0), None)
            .child(button(&MINIMIZE))
            .child(button(&MAXIMIZE))
            .child(button(&CLOSE))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_simulated_insets_take_the_three_buttons_and_the_band() {
        let insets = simulated_insets(ChromeInsets::default());
        assert_eq!(insets.trailing, 138.0);
        assert_eq!(insets.height, titlebar::HEIGHT);
        assert_eq!(insets.leading, 0.0);
    }
}
