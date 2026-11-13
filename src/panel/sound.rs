//! The Sound applet: choose which of PipeWire's sound cards sound goes to, and
//! set its volume (see `crate::sound`). PipeWire remembers both itself.

use smithay::{
    backend::renderer::{element::solid::SolidColorBuffer, gles::GlesRenderer},
    utils::{Logical, Point, Rectangle},
};

use super::{
    BTN_H, BUTTON, BUTTON_HOT, CELL_HOT, FIELD_BG, HEADER_H, NOTE_H, PAD, PanelHit, PanelId, Rect,
    SEPARATOR, bufs,
};
use crate::{
    sound::{self, Sink},
    text::TextRenderer,
    ui::{self, CustomElement},
};

// --- Layout (logical pixels) -----------------------------------------------

const SOUND_W: i32 = 520;
const CARD_H: i32 = 28;
const STEP_W: i32 = 32;
const PERCENT_W: i32 = 48;
/// How much − and + change the volume.
const STEP: f64 = 0.05;

const BAR_H: i32 = 8;
const THUMB_W: i32 = 6;

const VOLUME_FILL: [f32; 4] = [0.25, 0.45, 0.80, 1.0];
const THUMB: [f32; 4] = [0.90, 0.92, 0.96, 1.0];
const THUMB_HOT: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// The chosen output, when there are several to choose from.
const ROW_SELECTED: [f32; 4] = [0.25, 0.42, 0.70, 1.0];

#[derive(Default)]
pub struct SoundApplet {
    sinks: Vec<Sink>,
    /// The card sound goes to.
    default: Option<u32>,
    /// Its volume (1.0 is 100%).
    volume: Option<f64>,
    /// Why the cards or volume could not be read or set.
    error: Option<String>,
    solids: Vec<SolidColorBuffer>,
}

struct SoundLayout {
    header: Rect,
    cards: Vec<Rect>,
    separator: Rect,
    volume_label: Rect,
    down: Rect,
    bar: Rect,
    up: Rect,
    percent: Rect,
    note: Rect,
}

fn layout(c: Rect, cards: usize) -> SoundLayout {
    let x = c.loc.x + PAD;
    let w = c.size.w - 2 * PAD;
    let mut y = c.loc.y + PAD;
    let header = Rectangle::new((x, y).into(), (w, HEADER_H).into());
    y += HEADER_H;
    let cards = (0..cards.max(1) as i32)
        .map(|i| Rectangle::new((x, y + i * CARD_H).into(), (w, CARD_H).into()))
        .collect::<Vec<_>>();
    y += cards.len() as i32 * CARD_H + PAD;
    let separator = Rectangle::new((x, y).into(), (w, 1).into());
    y += 1 + PAD;
    let label_w = 64;
    let volume_label = Rectangle::new((x, y).into(), (label_w, BTN_H).into());
    let down = Rectangle::new((x + label_w, y).into(), (STEP_W, BTN_H).into());
    let bar_x = down.loc.x + STEP_W + 8;
    let bar_w = x + w - PERCENT_W - STEP_W - 8 - bar_x;
    let bar = Rectangle::new(
        (bar_x, y + (BTN_H - BAR_H) / 2).into(),
        (bar_w, BAR_H).into(),
    );
    let up = Rectangle::new((bar_x + bar_w + 8, y).into(), (STEP_W, BTN_H).into());
    let percent = Rectangle::new((x + w - PERCENT_W, y).into(), (PERCENT_W, BTN_H).into());
    y += BTN_H + 4;
    let note = Rectangle::new((x, y).into(), (w, NOTE_H).into());
    SoundLayout {
        header,
        cards,
        separator,
        volume_label,
        down,
        bar,
        up,
        percent,
        note,
    }
}

fn content_height(cards: usize) -> i32 {
    PAD + HEADER_H + cards.max(1) as i32 * CARD_H + PAD + 1 + PAD + BTN_H + 4 + NOTE_H + PAD
}

// --- Panels glue ---------------------------------------------------------------

impl super::Panels {
    pub(super) fn sound_size(&self) -> (i32, i32) {
        (SOUND_W, content_height(self.sound.sinks.len()))
    }

    fn sound_layout(&self) -> SoundLayout {
        layout(self.content_rect(PanelId::Sound), self.sound.sinks.len())
    }

    /// Re-read the cards and the volume (when the window is opened).
    pub fn refresh_sound(&mut self) {
        let s = &mut self.sound;
        s.error = None;
        match sound::outputs() {
            Ok(o) => {
                s.sinks = o.sinks;
                s.default = o.default;
            }
            Err(err) => {
                s.sinks.clear();
                s.default = None;
                s.error = Some(err);
            }
        }
        s.volume = None;
        if let Some(id) = s.default {
            match sound::volume(id) {
                Ok(v) => s.volume = Some(v),
                Err(err) => s.error = Some(err),
            }
        }
    }

    pub(super) fn sound_hit(&self, p: Point<f64, Logical>) -> Option<PanelHit> {
        let l = self.sound_layout();
        let inside = |r: Rect| r.to_f64().contains(p);
        // With only one output there is nothing to choose.
        if self.sound.sinks.len() > 1
            && let Some(i) = l
                .cards
                .iter()
                .take(self.sound.sinks.len())
                .position(|r| inside(*r))
        {
            return Some(PanelHit::SoundCard(i));
        }
        if self.sound.volume.is_some() {
            if inside(l.down) {
                return Some(PanelHit::SoundDown);
            }
            if inside(l.up) {
                return Some(PanelHit::SoundUp);
            }
            // The bar, plus a little above and below it so it is easy to hit.
            let target = Rectangle::new(
                (l.bar.loc.x, l.down.loc.y).into(),
                (l.bar.size.w, BTN_H).into(),
            );
            if inside(target) {
                let f = (p.x - l.bar.loc.x as f64) / l.bar.size.w.max(1) as f64;
                return Some(PanelHit::SoundBar(
                    (f.clamp(0.0, 1.0) * 100.0).round() as u32
                ));
            }
        }
        None
    }

    /// A click on card `i`: send sound to it.
    pub fn sound_choose(&mut self, i: usize) {
        let Some(id) = self.sound.sinks.get(i).map(|s| s.id) else {
            return;
        };
        if self.sound.default == Some(id) {
            return;
        }
        if let Err(err) = sound::set_default(id) {
            self.sound.error = Some(err);
            return;
        }
        self.refresh_sound();
    }

    /// Set the chosen card's volume (0.0 to 1.0).
    pub fn sound_set_volume(&mut self, volume: f64) {
        let Some(id) = self.sound.default else {
            return;
        };
        let volume = (volume.clamp(0.0, 1.0) * 100.0).round() / 100.0;
        match sound::set_volume(id, volume) {
            Ok(()) => {
                self.sound.volume = Some(volume);
                self.sound.error = None;
            }
            Err(err) => self.sound.error = Some(err),
        }
    }

    /// The − (`dir` -1) and + (`dir` 1) buttons.
    pub fn sound_step(&mut self, dir: f64) {
        if let Some(v) = self.sound.volume {
            self.sound_set_volume(v.min(1.0) + dir * STEP);
        }
    }

    pub(super) fn render_sound(
        &mut self,
        text: &mut TextRenderer,
        renderer: &mut GlesRenderer,
        hovered: Option<PanelHit>,
        elems: &mut Vec<CustomElement>,
    ) {
        let l = self.sound_layout();
        let SoundApplet {
            sinks,
            default,
            volume,
            error,
            solids,
        } = &mut self.sound;
        let n = sinks.len();
        // Buffers: one per card, separator, −, +, bar track, bar fill, thumb.
        let solids = bufs(solids, n + 6);

        let header = if n > 1 {
            "Sound output — click one to use it"
        } else {
            "Sound output"
        };
        elems.extend(ui::text_in(
            text,
            renderer,
            header,
            l.header.loc.x,
            l.header,
        ));
        if n == 0 {
            let r = l.cards[0];
            elems.extend(ui::text_in_alpha(
                text,
                renderer,
                "No sound outputs found",
                r.loc.x + 8,
                r,
                0.6,
            ));
        }
        let mut backs = Vec::new();
        for (i, (sink, r)) in sinks.iter().zip(&l.cards).enumerate() {
            let name = text.fit(&sink.description, r.size.w - 16);
            if n == 1 {
                // Nothing to choose: just say where sound goes.
                elems.extend(ui::text_in(text, renderer, &name, r.loc.x + 8, *r));
                backs.push((i, *r, FIELD_BG));
                continue;
            }
            let chosen = *default == Some(sink.id);
            let alpha = if chosen { 1.0 } else { 0.8 };
            elems.extend(ui::text_in_alpha(
                text,
                renderer,
                &name,
                r.loc.x + 8,
                *r,
                alpha,
            ));
            if chosen {
                backs.push((i, *r, ROW_SELECTED));
            } else if hovered == Some(PanelHit::SoundCard(i)) {
                backs.push((i, *r, CELL_HOT));
            }
        }
        for (i, r, color) in backs {
            elems.push(ui::solid(&mut solids[i], r, color));
        }
        elems.push(ui::solid(&mut solids[n], l.separator, SEPARATOR));

        elems.extend(ui::text_in(
            text,
            renderer,
            "Volume",
            l.volume_label.loc.x,
            l.volume_label,
        ));
        if let Some(v) = *volume {
            let percent = format!("{}%", (v * 100.0).round() as i64);
            elems.extend(ui::text_centered(text, renderer, &percent, l.percent));
            elems.extend(ui::text_centered(text, renderer, "−", l.down));
            elems.extend(ui::text_centered(text, renderer, "+", l.up));
            let button = |hit: PanelHit| {
                if hovered == Some(hit) {
                    BUTTON_HOT
                } else {
                    BUTTON
                }
            };
            elems.push(ui::solid(
                &mut solids[n + 1],
                l.down,
                button(PanelHit::SoundDown),
            ));
            elems.push(ui::solid(
                &mut solids[n + 2],
                l.up,
                button(PanelHit::SoundUp),
            ));
            let bar_hot = matches!(hovered, Some(PanelHit::SoundBar(_)));
            // The thumb sits on the end of the fill, kept inside the track.
            let fill_w = (v.clamp(0.0, 1.0) * l.bar.size.w as f64).round() as i32;
            let thumb_x = (l.bar.loc.x + fill_w - THUMB_W / 2)
                .clamp(l.bar.loc.x, l.bar.loc.x + l.bar.size.w - THUMB_W);
            let thumb = Rectangle::new(
                (thumb_x, l.down.loc.y + 4).into(),
                (THUMB_W, BTN_H - 8).into(),
            );
            elems.push(ui::solid(
                &mut solids[n + 5],
                thumb,
                if bar_hot { THUMB_HOT } else { THUMB },
            ));
            let fill = Rectangle::new(l.bar.loc, (fill_w, l.bar.size.h).into());
            elems.push(ui::solid(&mut solids[n + 4], fill, VOLUME_FILL));
            let track = if bar_hot { BUTTON } else { FIELD_BG };
            elems.push(ui::solid(&mut solids[n + 3], l.bar, track));
        }

        let (note, alpha) = match &*error {
            Some(err) => (err.clone(), 1.0),
            None if default.is_none() && n > 0 => {
                ("Choose an output to set its volume".to_string(), 0.45)
            }
            None => (String::new(), 0.0),
        };
        if !note.is_empty() {
            let note = text.fit(&note, l.note.size.w);
            elems.extend(ui::text_in_alpha(
                text,
                renderer,
                &note,
                l.note.loc.x,
                l.note,
                alpha,
            ));
        }
    }
}
