//! Thin scroll bars drawn by the window: rune scrolls but draws no bar. A bar
//! follows its viewport through live values, so the thumb moves in the same
//! frame as the content. It shows only while the content overflows, widens
//! and darkens under the pointer, drags, and pages when the track is pressed.

use super::{Dashboard, Palette};
use rune::{Drag, El, Live, Phase, ScrollPos, Tx, col, el};

/// Width of the strip that takes the pointer; the visible thumb is thinner.
pub const TRACK_WIDTH: f32 = 12.0;
/// Gap between the ends of the track and the viewport's, clear of rounded corners.
const TRACK_INSET: f32 = 8.0;
/// Gap between the track and the viewport's right edge.
const EDGE: f32 = 2.0;
const MIN_THUMB: f32 = 32.0;
const THIN: f32 = 4.0;
const WIDE: f32 = 8.0;

/// A thumb, in points from the top of its track.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thumb {
    pub top: f32,
    pub len: f32,
}

/// The thumb for a viewport of height `viewport` over `content` scrolled by
/// `offset`, in a track `track` points tall. None when the content fits.
pub fn thumb(track: f32, viewport: f32, content: f32, offset: f32) -> Option<Thumb> {
    let max = content - viewport;
    if max <= 0.5 || track <= 0.0 || viewport <= 0.0 {
        return None;
    }
    let len = (track * viewport / content).clamp(MIN_THUMB.min(track), track);
    let top = (track - len) * (offset / max).clamp(0.0, 1.0);
    Some(Thumb { top, len })
}

/// The scroll offset after dragging the thumb by `dy` points from where
/// `start_offset` had it: the thumb's travel maps onto the whole scroll range.
pub fn dragged_offset(track: f32, viewport: f32, content: f32, start_offset: f32, dy: f32) -> f32 {
    let max = (content - viewport).max(0.0);
    let Some(thumb) = thumb(track, viewport, content, start_offset) else {
        return 0.0;
    };
    let travel = track - thumb.len;
    if travel <= 0.0 {
        return start_offset.clamp(0.0, max);
    }
    (start_offset + dy * max / travel).clamp(0.0, max)
}

/// Which way pressing the track at `y` pages: up above the thumb, down below
/// it, and not at all on it.
pub fn page_direction(thumb: Thumb, y: f32) -> i32 {
    if y < thumb.top {
        -1
    } else if y > thumb.top + thumb.len {
        1
    } else {
        0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    List,
    Detail,
}

/// What a viewport reports, kept in live values so scrolling never re-runs the view.
#[derive(Clone)]
pub struct Meter {
    /// Offset, farthest offset and viewport height.
    pub(super) pos: Live<[f32; 4]>,
    /// The thumb in its track: `[x, y, width, height]`, drawn as it is.
    pub(super) rect: Live<[f32; 4]>,
    /// 1 while the content overflows, else 0.
    pub(super) shown: Live<f32>,
    /// 1 while the pointer is on the bar or holds the thumb: the thumb widens.
    wide: Live<f32>,
}

impl Meter {
    pub fn new() -> Self {
        Self {
            pos: Live::new([0.0; 4]),
            rect: Live::new([TRACK_WIDTH - THIN, 0.0, THIN, MIN_THUMB]),
            shown: Live::new(0.0),
            wide: Live::new(0.0),
        }
    }

    pub fn offset(&self) -> f32 {
        self.pos.get()[0]
    }

    /// For `El::on_scroll_offset` of the viewport this meter follows.
    pub fn follow(&self) -> impl Fn(ScrollPos) + 'static {
        let meter = self.clone();
        move |at| {
            meter
                .pos
                .set([at.offset[1], at.max[1], at.viewport[1], 0.0]);
            meter.refresh();
        }
    }

    /// Place the thumb for the last reported scroll position and width.
    fn refresh(&self) {
        let (track, viewport, content, offset) = self.geometry();
        match thumb(track, viewport, content, offset) {
            Some(thumb) => {
                let width = if self.wide.get() > 0.5 { WIDE } else { THIN };
                self.rect
                    .set([TRACK_WIDTH - width, thumb.top, width, thumb.len]);
                self.shown.set(1.0);
            }
            None => {
                self.shown.set(0.0);
            }
        }
    }

    fn set_wide(&self, wide: bool) {
        self.wide.set(if wide { 1.0 } else { 0.0 });
        self.refresh();
    }

    fn geometry(&self) -> (f32, f32, f32, f32) {
        let [offset, max, viewport, _] = self.pos.get();
        (track_height(viewport), viewport, viewport + max, offset)
    }
}

impl Default for Meter {
    fn default() -> Self {
        Self::new()
    }
}

fn track_height(viewport: f32) -> f32 {
    (viewport - 2.0 * TRACK_INSET).max(0.0)
}

/// Both bars and which one the pointer is on.
#[derive(Default)]
pub struct Bars {
    pub list: Meter,
    pub detail: Meter,
    hover: Option<Which>,
    grabbed: Option<Which>,
}

impl Bars {
    pub fn meter(&self, which: Which) -> &Meter {
        match which {
            Which::List => &self.list,
            Which::Detail => &self.detail,
        }
    }

    /// The pointer is over a bar or its thumb is held.
    pub fn is_active(&self, which: Which) -> bool {
        self.hover == Some(which) || self.grabbed == Some(which)
    }

    fn hovered(&mut self, which: Which, over: bool) {
        if over {
            self.hover = Some(which);
        } else if self.hover == Some(which) {
            self.hover = None;
        }
        self.meter(which).set_wide(self.is_active(which));
    }

    /// A press on the bar: on the thumb it grabs it, beside it it pages toward the
    /// pointer; then moves drag the thumb and the release lets go.
    fn track_drag(&mut self, which: Which, key: &str, drag: &mut Drag, tx: &mut Tx) {
        match drag.phase {
            Phase::Down => {
                let (track, viewport, content, offset) = self.meter(which).geometry();
                // Data is the offset the thumb was grabbed at; negative: nothing to drag.
                drag.data = -1.0;
                if let Some(thumb) = thumb(track, viewport, content, offset) {
                    match page_direction(thumb, drag.y) {
                        0 => {
                            drag.data = offset;
                            self.grabbed = Some(which);
                        }
                        direction => tx.scroll_page(key, direction),
                    }
                }
            }
            Phase::Move => {
                if drag.data >= 0.0 {
                    let (track, viewport, content, _) = self.meter(which).geometry();
                    let offset = dragged_offset(track, viewport, content, drag.data, drag.dy);
                    tx.scroll_to_instant(key, 0.0, offset);
                }
            }
            _ => self.grabbed = None,
        }
        self.meter(which).set_wide(self.is_active(which));
    }
}

/// The bar over the right edge of the viewport keyed `scroll_key`. Put it in
/// the viewport's container, after the viewport.
pub fn bar(which: Which, scroll_key: &'static str, state: &Dashboard, palette: &Palette) -> El {
    let t = palette.theme;
    let meter = state.bars.meter(which);
    let color = if state.bars.is_active(which) {
        t.text_secondary
    } else {
        t.text_secondary.alpha(0.7)
    };
    // The strip takes the pointer; the thumb is only drawn, at the rect the meter keeps.
    let thumb = el()
        .key(format!("{scroll_key}-polegar"))
        .absolute(TRACK_WIDTH - THIN, 0.0)
        .w(THIN)
        .h(MIN_THUMB)
        .radius(WIDE / 2.0)
        .fill(color)
        .live_rect(&meter.rect);
    col()
        .key(format!("{scroll_key}-barra"))
        .pin(None, Some(TRACK_INSET), Some(EDGE), Some(TRACK_INSET))
        .w(TRACK_WIDTH)
        .live_opacity(&meter.shown)
        .on_hover_change(move |s: &mut Dashboard, over, _: &mut Tx| s.bars.hovered(which, over))
        .on_drag(move |s: &mut Dashboard, drag: &mut Drag, tx: &mut Tx| {
            s.bars.track_drag(which, scroll_key, drag, tx);
        })
        .child(thumb)
}
