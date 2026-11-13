//! The Control Panel window (one icon per applet in `APPLETS`, plus Quit)
//! and the Help applet (a few static notes).

use smithay::{
    backend::renderer::gles::GlesRenderer,
    utils::{Logical, Point, Rectangle},
};

use super::{APPLETS, BTN_H, CELL_HOT, PAD, PanelHit, PanelId, Rect, SEPARATOR, bufs};
use crate::{
    text::TextRenderer,
    ui::{self, CustomElement},
};

// --- Layout (logical pixels) -----------------------------------------------

const CP_W: i32 = 640;
const CELL_W: i32 = 102;
const CELL_H: i32 = 92;
const TILE: i32 = 48;
const QUIT_W: i32 = 130;

const HELP_W: i32 = 460;
const HELP_ROW_H: i32 = 24;
const HELP_LINES: [&str; 4] = [
    "No flags or environment variables. Just run simplewm.",
    "The resolution is the monitor's preferred mode.",
    "There is no plain text config file.",
    "There are no keyboard shortcuts.",
];

// --- Colors ------------------------------------------------------------------

const QUIT: [f32; 4] = [0.55, 0.16, 0.16, 1.0];
const QUIT_HOT: [f32; 4] = [0.75, 0.22, 0.22, 1.0];
const TILE_LAUNCHER: [f32; 4] = [0.25, 0.45, 0.80, 1.0];
const TILE_ENVIRONMENT: [f32; 4] = [0.20, 0.58, 0.42, 1.0];
const TILE_HELP: [f32; 4] = [0.80, 0.50, 0.15, 1.0];
const TILE_SCREENSHOT: [f32; 4] = [0.55, 0.32, 0.75, 1.0];
const TILE_PORTAL: [f32; 4] = [0.15, 0.55, 0.65, 1.0];
const TILE_NETWORK: [f32; 4] = [0.70, 0.30, 0.35, 1.0];
const TILE_SOUND: [f32; 4] = [0.35, 0.55, 0.22, 1.0];
const TILE_BUS: [f32; 4] = [0.45, 0.40, 0.70, 1.0];
const TILE_GLYPH: [f32; 4] = [0.95, 0.97, 1.0, 1.0];

struct ControlLayout {
    cells: Vec<Rect>,
    separator: Rect,
    quit: Rect,
}

fn columns() -> usize {
    ((CP_W - 2 * PAD) / CELL_W).max(1) as usize
}

/// Max rects used to draw one applet glyph.
const GLYPH_MAX: usize = 9;

/// Glyph drawn on an applet's tile, and the tile color. A new applet needs
/// a glyph here (at most `GLYPH_MAX` rects, on a `TILE`-sized square).
fn applet_glyph(id: PanelId, tile: Rect) -> ([f32; 4], Vec<Rect>) {
    let (x, y) = (tile.loc.x, tile.loc.y);
    let r = |dx, dy, w, h| Rectangle::new((x + dx, y + dy).into(), (w, h).into());
    match id {
        // A 3x3 grid of app squares.
        PanelId::Launcher => {
            let (sq, gap) = (8, 5);
            let off = (TILE - (3 * sq + 2 * gap)) / 2;
            let rects = (0..9)
                .map(|k| {
                    r(
                        off + (k % 3) * (sq + gap),
                        off + (k / 3) * (sq + gap),
                        sq,
                        sq,
                    )
                })
                .collect();
            (TILE_LAUNCHER, rects)
        }
        // "KEY = value" lines: a short key bar and a longer value bar per line.
        PanelId::Environment => {
            let mut rects = Vec::new();
            for (row, value_w) in [(0, 18), (1, 12), (2, 22), (3, 15)] {
                rects.push(r(8, 10 + row * 8, 8, 4));
                rects.push(r(20, 10 + row * 8, value_w, 4));
            }
            (TILE_ENVIRONMENT, rects)
        }
        // A question mark: hook, stem and dot.
        PanelId::Help => {
            let rects = vec![
                r(16, 9, 4, 6),   // left tip of the hook
                r(16, 9, 16, 4),  // top
                r(28, 9, 4, 14),  // right side
                r(22, 19, 10, 4), // back toward the middle
                r(22, 19, 4, 10), // stem
                r(22, 33, 4, 4),  // dot
            ];
            (TILE_HELP, rects)
        }
        // Four corner brackets of a selection.
        PanelId::Screenshot => {
            let rects = vec![
                r(10, 10, 10, 3),
                r(10, 10, 3, 10),
                r(28, 10, 10, 3),
                r(35, 10, 3, 10),
                r(10, 35, 10, 3),
                r(10, 28, 3, 10),
                r(28, 35, 10, 3),
                r(35, 28, 3, 10),
            ];
            (TILE_SCREENSHOT, rects)
        }
        // A doorway: frame, threshold and an open door leaf.
        PanelId::Portal => {
            let rects = vec![
                r(14, 8, 20, 3),  // lintel
                r(14, 8, 3, 30),  // left jamb
                r(31, 8, 3, 30),  // right jamb
                r(10, 37, 28, 3), // threshold
                r(17, 13, 8, 24), // door leaf, swung open
                r(22, 24, 2, 3),  // handle gap
            ];
            (TILE_PORTAL, rects)
        }
        // Three linked machines: one on top wired down to two below.
        PanelId::Network => {
            let rects = vec![
                r(18, 8, 12, 9),  // top box
                r(23, 17, 2, 6),  // its cable
                r(11, 23, 26, 2), // bar
                r(11, 23, 2, 6),  // drop to the left box
                r(35, 23, 2, 6),  // drop to the right box
                r(6, 29, 12, 9),  // left box
                r(30, 29, 12, 9), // right box
            ];
            (TILE_NETWORK, rects)
        }
        // A speaker: box, stepped cone and two sound waves.
        PanelId::Sound => {
            let rects = vec![
                r(9, 19, 7, 10),  // box
                r(16, 15, 4, 18), // cone
                r(20, 11, 4, 26), // cone mouth
                r(28, 19, 3, 10), // small wave
                r(34, 14, 3, 20), // big wave
            ];
            (TILE_SOUND, rects)
        }
        // A bus line with three programs hanging off it.
        PanelId::Bus => {
            let rects = vec![
                r(6, 22, 36, 4), // the bus
                r(9, 10, 8, 8),  // program, top left
                r(12, 18, 2, 4), // its wire
                r(31, 10, 8, 8), // program, top right
                r(34, 18, 2, 4), // its wire
                r(20, 30, 8, 8), // program, below
                r(23, 26, 2, 4), // its wire
            ];
            (TILE_BUS, rects)
        }
        PanelId::ControlPanel | PanelId::FilePicker | PanelId::Password | PanelId::Log => {
            (TILE_LAUNCHER, Vec::new())
        }
    }
}

impl super::Panels {
    pub(super) fn control_size(&self) -> (i32, i32) {
        let rows = APPLETS.len().div_ceil(columns()) as i32;
        (CP_W, PAD + rows * CELL_H + PAD + 1 + PAD + BTN_H + PAD)
    }

    pub(super) fn help_size(&self) -> (i32, i32) {
        (HELP_W, PAD + HELP_LINES.len() as i32 * HELP_ROW_H + PAD)
    }

    fn control_layout(&self) -> ControlLayout {
        let c = self.content_rect(PanelId::ControlPanel);
        let cols = columns();
        let cells = (0..APPLETS.len())
            .map(|i| {
                let (col, row) = ((i % cols) as i32, (i / cols) as i32);
                Rectangle::new(
                    (c.loc.x + PAD + col * CELL_W, c.loc.y + PAD + row * CELL_H).into(),
                    (CELL_W, CELL_H).into(),
                )
            })
            .collect();
        let rows = APPLETS.len().div_ceil(cols) as i32;
        let y = c.loc.y + PAD + rows * CELL_H + PAD;
        let separator = Rectangle::new((c.loc.x + PAD, y).into(), (c.size.w - 2 * PAD, 1).into());
        let quit = Rectangle::new((c.loc.x + PAD, y + 1 + PAD).into(), (QUIT_W, BTN_H).into());
        ControlLayout {
            cells,
            separator,
            quit,
        }
    }

    pub(super) fn control_hit(&self, p: Point<f64, Logical>) -> Option<PanelHit> {
        let l = self.control_layout();
        if let Some(i) = l.cells.iter().position(|c| c.to_f64().contains(p)) {
            return Some(PanelHit::Applet(i));
        }
        l.quit.to_f64().contains(p).then_some(PanelHit::Quit)
    }

    pub(super) fn render_control(
        &mut self,
        text: &mut TextRenderer,
        renderer: &mut GlesRenderer,
        hovered: Option<PanelHit>,
        elems: &mut Vec<CustomElement>,
    ) {
        let l = self.control_layout();
        // Per applet: cell highlight, tile, glyph rects. Then separator, quit.
        let per = 2 + GLYPH_MAX;
        let s = bufs(
            &mut self.windows[PanelId::ControlPanel as usize].solids,
            APPLETS.len() * per + 2,
        );

        let mut back: Vec<CustomElement> = Vec::new();
        for (i, (applet, cell)) in APPLETS.iter().zip(&l.cells).enumerate() {
            let b = &mut s[i * per..(i + 1) * per];
            let tile = Rectangle::new(
                (cell.loc.x + (CELL_W - TILE) / 2, cell.loc.y + 8).into(),
                (TILE, TILE).into(),
            );
            let label = Rectangle::new(
                (cell.loc.x, tile.loc.y + TILE + 6).into(),
                (CELL_W, 20).into(),
            );
            elems.extend(ui::text_centered(text, renderer, applet.title(), label));
            let (tile_color, glyph) = applet_glyph(*applet, tile);
            for (k, r) in glyph.into_iter().take(GLYPH_MAX).enumerate() {
                elems.push(ui::solid(&mut b[2 + k], r, TILE_GLYPH));
            }
            elems.push(ui::solid(&mut b[1], tile, tile_color));
            if hovered == Some(PanelHit::Applet(i)) {
                back.push(ui::solid(&mut b[0], *cell, CELL_HOT));
            }
        }
        elems.append(&mut back);

        let n = APPLETS.len() * per;
        elems.extend(ui::text_centered(text, renderer, "Quit simplewm", l.quit));
        let qc = if hovered == Some(PanelHit::Quit) {
            QUIT_HOT
        } else {
            QUIT
        };
        elems.push(ui::solid(&mut s[n], l.quit, qc));
        elems.push(ui::solid(&mut s[n + 1], l.separator, SEPARATOR));
    }

    pub(super) fn render_help(
        &self,
        text: &mut TextRenderer,
        renderer: &mut GlesRenderer,
        elems: &mut Vec<CustomElement>,
    ) {
        let c = self.content_rect(PanelId::Help);
        for (i, line) in HELP_LINES.iter().enumerate() {
            let r = Rectangle::new(
                (c.loc.x + PAD, c.loc.y + PAD + i as i32 * HELP_ROW_H).into(),
                (c.size.w - 2 * PAD, HELP_ROW_H).into(),
            );
            elems.extend(ui::text_in(text, renderer, line, r.loc.x, r));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_applet_glyph_fits_its_tile() {
        let tile = Rectangle::new((0, 0).into(), (TILE, TILE).into());
        for id in APPLETS {
            let name = id.title();
            let (_, rects) = applet_glyph(id, tile);
            assert!(!rects.is_empty(), "{name} has no glyph");
            assert!(rects.len() <= GLYPH_MAX, "{name} glyph has too many rects");
            for r in rects {
                assert!(tile.contains_rect(r), "{name} glyph leaves its tile: {r:?}");
            }
        }
    }
}
