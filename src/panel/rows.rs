//! Scrollable read-only row lists, and the two applets that are just one:
//! Environment (how simplewm was started, see `crate::session_env`) and
//! Desktop Portal (file dialog checks, see `crate::portal_check`, plus a
//! Refresh button). The Network applet uses a `RowList` too.

use smithay::{
    backend::renderer::{element::solid::SolidColorBuffer, gles::GlesRenderer},
    utils::{Logical, Point, Rectangle},
};

use super::{
    BTN_H, BUTTON, BUTTON_HOT, PAD, PanelHit, PanelId, Rect, SCROLL_THUMB, SCROLL_TRACK,
    SCROLLBAR_W, buf, bufs,
};
use crate::{
    text::TextRenderer,
    ui::{self, CustomElement},
};

// --- Layout (logical pixels) -----------------------------------------------

pub(super) const ENV_ROW_H: i32 = 20;
const ENV_KEY_W: i32 = 230;

// Environment applet
pub(super) const ENV_W: i32 = 700;
pub(super) const ENV_H: i32 = 520;

// Desktop Portal applet
pub(super) const PORTAL_W: i32 = 760;
pub(super) const PORTAL_H: i32 = 560;
const REFRESH_W: i32 = 100;

const ENV_HEADER_BG: [f32; 4] = [0.19, 0.21, 0.26, 1.0];

/// A row in the Environment, Desktop Portal or Network window.
#[derive(Debug, Clone)]
pub enum EnvRow {
    Header(String),
    Item(String, String),
    /// Text across the whole row.
    Line(String),
}

/// A scrollable list of rows.
#[derive(Default)]
pub(super) struct RowList {
    pub(super) rows: Vec<EnvRow>,
    scroll: usize,
    scroll_acc: f64,
}

impl RowList {
    fn visible(vp: Rect) -> usize {
        (vp.size.h / ENV_ROW_H).max(1) as usize
    }

    fn max_scroll(&self, vp: Rect) -> usize {
        self.rows.len().saturating_sub(Self::visible(vp))
    }

    pub(super) fn set(&mut self, rows: Vec<EnvRow>, vp: Rect) {
        self.rows = rows;
        self.scroll = self.scroll.min(self.max_scroll(vp));
    }

    /// Scroll by `amount` (pointer axis units; positive scrolls down).
    pub(super) fn scroll_by(&mut self, amount: f64, vp: Rect) {
        self.scroll_acc += amount;
        let rows = (self.scroll_acc / ENV_ROW_H as f64).trunc();
        if rows != 0.0 {
            self.scroll_acc -= rows * ENV_ROW_H as f64;
            let next = (self.scroll as i64 + rows as i64).clamp(0, self.max_scroll(vp) as i64);
            self.scroll = next as usize;
        }
    }

    pub(super) fn render(
        &self,
        vp: Rect,
        solids: &mut Vec<SolidColorBuffer>,
        text: &mut TextRenderer,
        renderer: &mut GlesRenderer,
        elems: &mut Vec<CustomElement>,
    ) {
        let visible = Self::visible(vp);
        let total = self.rows.len();
        let scrollable = total > visible;
        let text_w = vp.size.w - if scrollable { SCROLLBAR_W + 8 } else { 0 };
        let value_x = vp.loc.x + ENV_KEY_W + 12;
        let value_w = vp.loc.x + text_w - value_x;
        let max_scroll = self.max_scroll(vp).max(1) as f64;

        // Buffers: one header background per visible row, then track + thumb.
        let s = bufs(solids, visible + 2);
        let mut backs = Vec::new();
        for (slot, row) in self.rows.iter().skip(self.scroll).take(visible).enumerate() {
            let r = Rectangle::new(
                (vp.loc.x, vp.loc.y + slot as i32 * ENV_ROW_H).into(),
                (text_w, ENV_ROW_H).into(),
            );
            match row {
                EnvRow::Header(title) => {
                    elems.extend(ui::text_in(text, renderer, title, r.loc.x + 6, r));
                    backs.push((slot, r));
                }
                EnvRow::Item(key, value) => {
                    let key = text.fit(key, ENV_KEY_W);
                    elems.extend(ui::text_in_alpha(text, renderer, &key, r.loc.x + 6, r, 0.7));
                    let value = text.fit(value, value_w);
                    elems.extend(ui::text_in(text, renderer, &value, value_x, r));
                }
                EnvRow::Line(line) => {
                    let line = text.fit(line, text_w - 12);
                    elems.extend(ui::text_in(text, renderer, &line, r.loc.x + 6, r));
                }
            }
        }
        for (slot, r) in backs {
            elems.push(ui::solid(&mut s[slot], r, ENV_HEADER_BG));
        }

        if scrollable {
            let track = Rectangle::new(
                (vp.loc.x + vp.size.w - SCROLLBAR_W, vp.loc.y).into(),
                (SCROLLBAR_W, vp.size.h).into(),
            );
            let thumb_h = ((visible as f64 / total as f64) * track.size.h as f64).max(20.0) as i32;
            let thumb_y = track.loc.y
                + ((self.scroll as f64 / max_scroll) * (track.size.h - thumb_h) as f64) as i32;
            let thumb =
                Rectangle::new((track.loc.x, thumb_y).into(), (SCROLLBAR_W, thumb_h).into());
            elems.push(ui::solid(&mut s[visible + 1], thumb, SCROLL_THUMB));
            elems.push(ui::solid(&mut s[visible], track, SCROLL_TRACK));
        }
    }
}

impl super::Panels {
    /// Replace the Environment rows (done each time the window is opened).
    pub fn set_environment(&mut self, rows: Vec<EnvRow>) {
        let vp = self.env_viewport();
        self.env.set(rows, vp);
    }

    /// Replace the Desktop Portal rows (check results).
    pub fn set_portal_rows(&mut self, rows: Vec<EnvRow>) {
        let vp = self.portal_viewport();
        self.portal_rows.set(rows, vp);
    }

    fn env_viewport(&self) -> Rect {
        let c = self.content_rect(PanelId::Environment);
        Rectangle::new(
            (c.loc.x + PAD, c.loc.y + PAD).into(),
            (c.size.w - 2 * PAD, c.size.h - 2 * PAD).into(),
        )
    }

    /// Rows area of the Desktop Portal window (the Refresh button is below it).
    fn portal_viewport(&self) -> Rect {
        let c = self.content_rect(PanelId::Portal);
        Rectangle::new(
            (c.loc.x + PAD, c.loc.y + PAD).into(),
            (c.size.w - 2 * PAD, c.size.h - 3 * PAD - BTN_H).into(),
        )
    }

    fn portal_refresh(&self) -> Rect {
        let c = self.content_rect(PanelId::Portal);
        Rectangle::new(
            (c.loc.x + PAD, c.loc.y + c.size.h - PAD - BTN_H).into(),
            (REFRESH_W, BTN_H).into(),
        )
    }

    pub(super) fn portal_hit(&self, p: Point<f64, Logical>) -> Option<PanelHit> {
        self.portal_refresh()
            .to_f64()
            .contains(p)
            .then_some(PanelHit::Refresh)
    }

    /// Scroll a row list window (Environment, Desktop Portal, Network) by
    /// `amount` (pointer axis units; positive scrolls down).
    pub fn scroll_rows(&mut self, id: PanelId, amount: f64) {
        match id {
            PanelId::Environment => {
                let vp = self.env_viewport();
                self.env.scroll_by(amount, vp);
            }
            PanelId::Portal => {
                let vp = self.portal_viewport();
                self.portal_rows.scroll_by(amount, vp);
            }
            PanelId::Network => self.scroll_network(amount),
            _ => {}
        }
    }

    pub(super) fn render_environment(
        &mut self,
        text: &mut TextRenderer,
        renderer: &mut GlesRenderer,
        elems: &mut Vec<CustomElement>,
    ) {
        let vp = self.env_viewport();
        self.env
            .render(vp, &mut self.environment.solids, text, renderer, elems);
    }

    pub(super) fn render_portal(
        &mut self,
        text: &mut TextRenderer,
        renderer: &mut GlesRenderer,
        hovered: Option<PanelHit>,
        elems: &mut Vec<CustomElement>,
    ) {
        let vp = self.portal_viewport();
        let refresh = self.portal_refresh();
        self.portal_rows
            .render(vp, &mut self.portal.solids, text, renderer, elems);
        elems.extend(ui::text_centered(text, renderer, "Refresh", refresh));
        let rc = if hovered == Some(PanelHit::Refresh) {
            BUTTON_HOT
        } else {
            BUTTON
        };
        elems.push(ui::solid(buf(&mut self.portal_refresh_buf), refresh, rc));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(n: usize) -> Vec<EnvRow> {
        (0..n).map(|i| EnvRow::Line(i.to_string())).collect()
    }

    #[test]
    fn scrolls_within_bounds() {
        // 5 rows fit; 12 rows means up to 7 rows of scroll.
        let vp = Rectangle::new((0, 0).into(), (100, 5 * ENV_ROW_H).into());
        let mut list = RowList::default();
        list.set(rows(12), vp);
        list.scroll_by(-100.0, vp);
        assert_eq!(list.scroll, 0);
        // Less than a row accumulates.
        list.scroll_by(ENV_ROW_H as f64 / 2.0, vp);
        assert_eq!(list.scroll, 0);
        list.scroll_by(ENV_ROW_H as f64 / 2.0, vp);
        assert_eq!(list.scroll, 1);
        list.scroll_by(1000.0, vp);
        assert_eq!(list.scroll, 7);
        // Fewer rows: the scroll is pulled back.
        list.set(rows(8), vp);
        assert_eq!(list.scroll, 3);
        list.set(rows(2), vp);
        assert_eq!(list.scroll, 0);
    }
}
