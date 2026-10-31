//! Compositor-drawn settings windows ("panels"), Windows Control Panel style:
//!
//! * The Control Panel window shows one icon per settings applet, plus a
//!   Quit button. Clicking an icon opens that applet's own window.
//! * The Launcher applet window edits the list of executable paths shown in
//!   the Launcher menu.
//! * The Environment applet window shows (read-only) how simplewm was started:
//!   Wayland socket, backend devices, protocols, and environment variables.
//!
//! Panels stack, focus and move like client windows. Settings are stored in
//! SQLite at ~/.config/simplewm/simplewm.db.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use smithay::{
    backend::renderer::{element::solid::SolidColorBuffer, gles::GlesRenderer},
    input::keyboard::keysyms,
    utils::{Logical, Point, Rectangle},
};

use crate::{
    text::TextRenderer,
    ui::{self, CustomElement, Frame},
};

pub const DEFAULT_LAUNCHER_ENTRY: &str = "/usr/bin/alacritty";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelId {
    ControlPanel,
    Launcher,
    Environment,
}

impl PanelId {

    pub fn title(self) -> &'static str {
        match self {
            PanelId::ControlPanel => "Control Panel",
            PanelId::Launcher => "Launcher",
            PanelId::Environment => "Environment",
        }
    }
}

/// Applets shown as icons in the Control Panel: (name, window it opens).
pub const APPLETS: [(&str, PanelId); 2] = [
    ("Launcher", PanelId::Launcher),
    ("Environment", PanelId::Environment),
];

// --- Layout (logical pixels) -----------------------------------------------

const PAD: i32 = 14;
const BTN_H: i32 = 26;

// Control Panel
const CP_W: i32 = 440;
const CELL_W: i32 = 100;
const CELL_H: i32 = 92;
const TILE: i32 = 48;
const QUIT_W: i32 = 130;

// Launcher applet
const LAUNCHER_W: i32 = 520;
const HEADER_H: i32 = 26;
const ROW_H: i32 = 32;
const FIELD_H: i32 = 26;
const FIELD_TEXT_PAD: i32 = 6;
const REMOVE_W: i32 = 76;
const ADD_W: i32 = 90;

// Environment applet
const ENV_W: i32 = 700;
const ENV_H: i32 = 520;
const ENV_ROW_H: i32 = 20;
const ENV_KEY_W: i32 = 230;
const SCROLLBAR_W: i32 = 6;

// --- Colors (premultiplied RGBA) -------------------------------------------

const BG: [f32; 4] = [0.13, 0.14, 0.17, 1.0];
const FIELD_BG: [f32; 4] = [0.08, 0.09, 0.11, 1.0];
const FIELD_BG_EDIT: [f32; 4] = [0.05, 0.06, 0.08, 1.0];
const FIELD_EDIT_RING: [f32; 4] = [0.25, 0.42, 0.70, 1.0];
const BUTTON: [f32; 4] = [0.22, 0.24, 0.29, 1.0];
const BUTTON_HOT: [f32; 4] = [0.32, 0.35, 0.42, 1.0];
const QUIT: [f32; 4] = [0.55, 0.16, 0.16, 1.0];
const QUIT_HOT: [f32; 4] = [0.75, 0.22, 0.22, 1.0];
const SEPARATOR: [f32; 4] = [0.25, 0.27, 0.31, 1.0];
const CARET: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const CELL_HOT: [f32; 4] = [0.20, 0.24, 0.32, 1.0];
const TILE_LAUNCHER: [f32; 4] = [0.25, 0.45, 0.80, 1.0];
const TILE_ENVIRONMENT: [f32; 4] = [0.20, 0.58, 0.42, 1.0];
const ENV_HEADER_BG: [f32; 4] = [0.19, 0.21, 0.26, 1.0];
const SCROLL_TRACK: [f32; 4] = [0.18, 0.19, 0.23, 1.0];
const SCROLL_THUMB: [f32; 4] = [0.40, 0.43, 0.50, 1.0];
const TILE_GLYPH: [f32; 4] = [0.95, 0.97, 1.0, 1.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelHit {
    Title,
    Close,
    Body,
    // Control Panel
    Applet(usize),
    Quit,
    // Launcher applet: text field `.0`, with the char index under the pointer.
    Field(usize, usize),
    Remove(usize),
    Add,
}

type Rect = Rectangle<i32, Logical>;

fn new_buf() -> SolidColorBuffer {
    SolidColorBuffer::new((0, 0), [0.0; 4])
}

fn buf(slot: &mut Option<SolidColorBuffer>) -> &mut SolidColorBuffer {
    slot.get_or_insert_with(new_buf)
}

/// Grow/shrink a list of buffers to `n`.
fn bufs(v: &mut Vec<SolidColorBuffer>, n: usize) -> &mut Vec<SolidColorBuffer> {
    v.resize_with(n, new_buf);
    v
}

#[derive(Default)]
struct FrameBuffers {
    bg: Option<SolidColorBuffer>,
    title: Option<SolidColorBuffer>,
    left: Option<SolidColorBuffer>,
    right: Option<SolidColorBuffer>,
    bottom: Option<SolidColorBuffer>,
    close: Option<SolidColorBuffer>,
}

/// Window state shared by all panels.
#[derive(Default)]
pub struct PanelWindow {
    pub open: bool,
    /// Workspace (0 or 1) the panel was opened on.
    pub workspace: usize,
    /// Top-left of the content area (set when first shown).
    pub loc: Option<Point<i32, Logical>>,
    frame: FrameBuffers,
    /// Buffers for the panel's own contents.
    solids: Vec<SolidColorBuffer>,
}

// --- Layouts -----------------------------------------------------------------

struct ControlLayout {
    cells: Vec<Rect>,
    separator: Rect,
    quit: Rect,
}

struct LauncherLayout {
    header: Rect,
    fields: Vec<(Rect, Rect)>,
    add: Rect,
}

/// A row in the Environment window.
#[derive(Debug, Clone)]
pub enum EnvRow {
    Header(String),
    Item(String, String),
}

/// Max rects used to draw one applet glyph.
const GLYPH_MAX: usize = 9;

/// Glyph drawn on an applet's tile, and the tile color.
fn applet_glyph(id: PanelId, tile: Rect) -> ([f32; 4], Vec<Rect>) {
    match id {
        // A 3x3 grid of app squares.
        PanelId::Launcher => {
            let (sq, gap) = (8, 5);
            let off = (TILE - (3 * sq + 2 * gap)) / 2;
            let rects = (0..9)
                .map(|k| {
                    let (gx, gy) = (k % 3, k / 3);
                    Rectangle::new(
                        (tile.loc.x + off + gx * (sq + gap), tile.loc.y + off + gy * (sq + gap)).into(),
                        (sq, sq).into(),
                    )
                })
                .collect();
            (TILE_LAUNCHER, rects)
        }
        // "KEY = value" lines: a short key bar and a longer value bar per line.
        PanelId::Environment => {
            let mut rects = Vec::new();
            for (row, value_w) in [(0, 18), (1, 12), (2, 22), (3, 15)] {
                let y = tile.loc.y + 10 + row * 8;
                rects.push(Rectangle::new((tile.loc.x + 8, y).into(), (8, 4).into()));
                rects.push(Rectangle::new((tile.loc.x + 20, y).into(), (value_w, 4).into()));
            }
            (TILE_ENVIRONMENT, rects)
        }
        PanelId::ControlPanel => (TILE_LAUNCHER, Vec::new()),
    }
}

pub struct Panels {
    control: PanelWindow,
    launcher: PanelWindow,
    environment: PanelWindow,
    /// Environment rows (refreshed when the window is opened) and scroll state.
    env_rows: Vec<EnvRow>,
    env_scroll: usize,
    env_scroll_acc: f64,
    /// Panel with keyboard focus (instead of a client window).
    pub active: Option<PanelId>,

    // Launcher settings
    pub entries: Vec<String>,
    /// Field being edited and the caret position (in chars).
    pub editing: Option<usize>,
    pub cursor: usize,
    db: Option<Connection>,
}

impl Panels {
    pub fn new() -> Self {
        let db = open_db();
        let entries = match &db {
            Some(conn) => load_entries(conn),
            None => vec![DEFAULT_LAUNCHER_ENTRY.to_string()],
        };
        Self {
            control: PanelWindow::default(),
            launcher: PanelWindow::default(),
            environment: PanelWindow::default(),
            env_rows: Vec::new(),
            env_scroll: 0,
            env_scroll_acc: 0.0,
            active: None,
            entries,
            editing: None,
            cursor: 0,
            db,
        }
    }

    fn win(&self, id: PanelId) -> &PanelWindow {
        match id {
            PanelId::ControlPanel => &self.control,
            PanelId::Launcher => &self.launcher,
            PanelId::Environment => &self.environment,
        }
    }

    fn win_mut(&mut self, id: PanelId) -> &mut PanelWindow {
        match id {
            PanelId::ControlPanel => &mut self.control,
            PanelId::Launcher => &mut self.launcher,
            PanelId::Environment => &mut self.environment,
        }
    }

    /// Open and on the given workspace.
    pub fn is_visible(&self, id: PanelId, workspace: usize) -> bool {
        self.win(id).open && self.win(id).workspace == workspace
    }

    pub fn loc(&self, id: PanelId) -> Option<Point<i32, Logical>> {
        self.win(id).loc
    }

    pub fn set_loc(&mut self, id: PanelId, loc: Point<i32, Logical>) {
        self.win_mut(id).loc = Some(loc);
    }

    /// Non-empty launcher paths, in order.
    pub fn launcher_paths(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty())
            .collect()
    }

    // --- geometry -------------------------------------------------------------

    fn content_size(&self, id: PanelId) -> (i32, i32) {
        match id {
            PanelId::ControlPanel => {
                let cols = ((CP_W - 2 * PAD) / CELL_W).max(1) as usize;
                let rows = APPLETS.len().div_ceil(cols) as i32;
                (CP_W, PAD + rows * CELL_H + PAD + 1 + PAD + BTN_H + PAD)
            }
            PanelId::Launcher => {
                let rows = self.entries.len() as i32;
                (LAUNCHER_W, PAD + HEADER_H + rows * ROW_H + 4 + BTN_H + PAD)
            }
            PanelId::Environment => (ENV_W, ENV_H),
        }
    }

    pub fn content_rect(&self, id: PanelId) -> Rect {
        Rectangle::new(self.win(id).loc.unwrap_or_default(), self.content_size(id).into())
    }

    fn frame(&self, id: PanelId, text: &TextRenderer) -> Frame {
        Frame::new(self.content_rect(id), text)
    }

    fn control_layout(&self) -> ControlLayout {
        let c = self.content_rect(PanelId::ControlPanel);
        let cols = ((CP_W - 2 * PAD) / CELL_W).max(1) as usize;
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
        ControlLayout { cells, separator, quit }
    }

    fn launcher_layout(&self) -> LauncherLayout {
        let c = self.content_rect(PanelId::Launcher);
        let x = c.loc.x + PAD;
        let inner_w = c.size.w - 2 * PAD;
        let mut y = c.loc.y + PAD;
        let header = Rectangle::new((x, y).into(), (inner_w, HEADER_H).into());
        y += HEADER_H;
        let field_w = inner_w - REMOVE_W - 8;
        let mut fields = Vec::new();
        for _ in &self.entries {
            let field = Rectangle::new((x, y).into(), (field_w, FIELD_H).into());
            let remove = Rectangle::new((x + field_w + 8, y).into(), (REMOVE_W, FIELD_H).into());
            fields.push((field, remove));
            y += ROW_H;
        }
        let add = Rectangle::new((x, y + 4).into(), (ADD_W, BTN_H).into());
        LauncherLayout { header, fields, add }
    }

    /// Open a panel on `workspace` (moving it there if it's open elsewhere).
    /// The first time, the Control Panel is centered and applet windows open
    /// offset from it.
    pub fn show(&mut self, id: PanelId, out: Rect, workspace: usize) {
        self.win_mut(id).workspace = workspace;
        if self.win(id).loc.is_none() {
            let (w, h) = self.content_size(id);
            let min_y = out.loc.y + ui::BAR_HEIGHT + ui::TITLE_HEIGHT;
            let loc = match (id, self.control.loc) {
                (PanelId::ControlPanel, _) | (_, None) => Point::from((
                    out.loc.x + (out.size.w - w) / 2,
                    (out.loc.y + (out.size.h - h) / 3).max(min_y),
                )),
                (_, Some(cp)) => cp + Point::from((40, 40)),
            };
            self.win_mut(id).loc = Some(loc);
        }
        self.win_mut(id).open = true;
    }

    pub fn close(&mut self, id: PanelId) {
        if id == PanelId::Launcher {
            self.stop_editing();
        }
        self.win_mut(id).open = false;
        if self.active == Some(id) {
            self.active = None;
        }
    }

    pub fn hit(&self, id: PanelId, text: &TextRenderer, p: Point<f64, Logical>) -> Option<PanelHit> {
        if !self.win(id).open {
            return None;
        }
        let f = self.frame(id, text);
        if !f.frame.to_f64().contains(p) {
            return None;
        }
        if f.close.to_f64().contains(p) {
            return Some(PanelHit::Close);
        }
        if f.title.to_f64().contains(p) {
            return Some(PanelHit::Title);
        }
        match id {
            PanelId::ControlPanel => {
                let l = self.control_layout();
                if let Some(i) = l.cells.iter().position(|c| c.to_f64().contains(p)) {
                    return Some(PanelHit::Applet(i));
                }
                if l.quit.to_f64().contains(p) {
                    return Some(PanelHit::Quit);
                }
            }
            PanelId::Launcher => {
                let l = self.launcher_layout();
                for (i, (field, remove)) in l.fields.iter().enumerate() {
                    if field.to_f64().contains(p) {
                        return Some(PanelHit::Field(i, self.char_at(text, i, *field, p.x)));
                    }
                    if remove.to_f64().contains(p) {
                        return Some(PanelHit::Remove(i));
                    }
                }
                if l.add.to_f64().contains(p) {
                    return Some(PanelHit::Add);
                }
            }
            PanelId::Environment => {}
        }
        Some(PanelHit::Body)
    }

    // --- Launcher editing -----------------------------------------------------

    pub fn begin_edit(&mut self, field: usize, cursor: usize) {
        if self.editing.is_some() && self.editing != Some(field) {
            self.save();
        }
        self.editing = Some(field);
        self.cursor = cursor.min(self.entries[field].chars().count());
    }

    pub fn stop_editing(&mut self) {
        if self.editing.take().is_some() {
            self.save();
        }
    }

    pub fn add_entry(&mut self) {
        self.entries.push(String::new());
        let i = self.entries.len() - 1;
        self.begin_edit(i, 0);
    }

    pub fn remove_entry(&mut self, i: usize) {
        if i >= self.entries.len() {
            return;
        }
        self.entries.remove(i);
        self.editing = match self.editing {
            Some(e) if e == i => None,
            Some(e) if e > i => Some(e - 1),
            other => other,
        };
        self.save();
    }

    /// Handle a key press while a Launcher field is being edited.
    pub fn key(&mut self, sym: u32, ch: Option<char>, ctrl: bool) {
        let Some(i) = self.editing else {
            return;
        };
        let len = self.entries[i].chars().count();
        let mut changed = false;
        match sym {
            keysyms::KEY_BackSpace => {
                if self.cursor > 0 {
                    remove_char(&mut self.entries[i], self.cursor - 1);
                    self.cursor -= 1;
                    changed = true;
                }
            }
            keysyms::KEY_Delete | keysyms::KEY_KP_Delete => {
                if self.cursor < len {
                    remove_char(&mut self.entries[i], self.cursor);
                    changed = true;
                }
            }
            keysyms::KEY_Left | keysyms::KEY_KP_Left => self.cursor = self.cursor.saturating_sub(1),
            keysyms::KEY_Right | keysyms::KEY_KP_Right => self.cursor = (self.cursor + 1).min(len),
            keysyms::KEY_Home | keysyms::KEY_KP_Home => self.cursor = 0,
            keysyms::KEY_End | keysyms::KEY_KP_End => self.cursor = len,
            keysyms::KEY_Return | keysyms::KEY_KP_Enter | keysyms::KEY_Escape => self.stop_editing(),
            keysyms::KEY_Tab | keysyms::KEY_Down | keysyms::KEY_KP_Down => {
                let next = (i + 1) % self.entries.len();
                let c = self.cursor;
                self.begin_edit(next, c);
            }
            keysyms::KEY_ISO_Left_Tab | keysyms::KEY_Up | keysyms::KEY_KP_Up => {
                let prev = (i + self.entries.len() - 1) % self.entries.len();
                let c = self.cursor;
                self.begin_edit(prev, c);
            }
            _ => {
                if let Some(c) = ch {
                    if !ctrl && !c.is_control() {
                        insert_char(&mut self.entries[i], self.cursor, c);
                        self.cursor += 1;
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.save();
        }
    }

    fn char_at(&self, text: &TextRenderer, i: usize, field: Rect, x: f64) -> usize {
        let s = &self.entries[i];
        let cursor = (self.editing == Some(i)).then_some(self.cursor);
        let view = field_view(text, s, cursor, field.size.w - 2 * FIELD_TEXT_PAD);
        let mut pos = (field.loc.x + FIELD_TEXT_PAD) as f64;
        let mut idx = view.start;
        for c in s.chars().skip(view.start) {
            let adv = text.measure(&c.to_string()) as f64;
            if x < pos + adv / 2.0 {
                break;
            }
            pos += adv;
            idx += 1;
        }
        idx
    }

    fn save(&mut self) {
        let Some(conn) = self.db.as_mut() else {
            return;
        };
        let paths: Vec<String> = self
            .entries
            .iter()
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty())
            .collect();
        let result = (|| -> rusqlite::Result<()> {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM launcher", [])?;
            for (i, p) in paths.iter().enumerate() {
                tx.execute("INSERT INTO launcher (position, path) VALUES (?1, ?2)", (i as i64, p))?;
            }
            tx.commit()
        })();
        if let Err(err) = result {
            tracing::error!(?err, "failed to save launcher entries");
        }
    }

    // --- rendering ------------------------------------------------------------

    /// Push a panel's render elements (front-most first).
    pub fn render(
        &mut self,
        id: PanelId,
        text: &mut TextRenderer,
        renderer: &mut GlesRenderer,
        hovered: Option<PanelHit>,
        elems: &mut Vec<CustomElement>,
    ) {
        if !self.win(id).open {
            return;
        }
        match id {
            PanelId::ControlPanel => self.render_control(text, renderer, hovered, elems),
            PanelId::Launcher => self.render_launcher(text, renderer, hovered, elems),
            PanelId::Environment => self.render_environment(text, renderer, elems),
        }

        // Frame: content background, title bar with close button, borders.
        let f = self.frame(id, text);
        let active = self.active == Some(id);
        let fb = &mut self.win_mut(id).frame;
        elems.push(ui::solid(buf(&mut fb.bg), f.content, BG));
        elems.extend(ui::text_in(text, renderer, id.title(), f.title_text_x, f.title));
        elems.extend(ui::text_centered(text, renderer, "×", f.close));
        let title_color = if active { ui::TITLE_FOCUSED } else { ui::TITLE_UNFOCUSED };
        let cc = if hovered == Some(PanelHit::Close) { ui::CLOSE_HOT } else { ui::CLOSE_COLOR };
        elems.push(ui::solid(buf(&mut fb.close), f.close, cc));
        elems.push(ui::solid(buf(&mut fb.title), f.title, title_color));
        elems.push(ui::solid(buf(&mut fb.left), f.left, title_color));
        elems.push(ui::solid(buf(&mut fb.right), f.right, title_color));
        elems.push(ui::solid(buf(&mut fb.bottom), f.bottom, title_color));
    }

    fn render_control(
        &mut self,
        text: &mut TextRenderer,
        renderer: &mut GlesRenderer,
        hovered: Option<PanelHit>,
        elems: &mut Vec<CustomElement>,
    ) {
        let l = self.control_layout();
        // Per applet: cell highlight, tile, glyph rects. Then separator, quit.
        let per = 2 + GLYPH_MAX;
        let s = bufs(&mut self.control.solids, APPLETS.len() * per + 2);

        let mut back: Vec<CustomElement> = Vec::new();
        for (i, ((name, applet), cell)) in APPLETS.iter().zip(&l.cells).enumerate() {
            let b = &mut s[i * per..(i + 1) * per];
            let tile = Rectangle::new((cell.loc.x + (CELL_W - TILE) / 2, cell.loc.y + 8).into(), (TILE, TILE).into());
            let label = Rectangle::new((cell.loc.x, tile.loc.y + TILE + 6).into(), (CELL_W, 20).into());
            elems.extend(ui::text_centered(text, renderer, name, label));
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
        let qc = if hovered == Some(PanelHit::Quit) { QUIT_HOT } else { QUIT };
        elems.push(ui::solid(&mut s[n], l.quit, qc));
        elems.push(ui::solid(&mut s[n + 1], l.separator, SEPARATOR));
    }

    // --- Environment ----------------------------------------------------------

    /// Replace the Environment rows (done each time the window is opened).
    pub fn set_environment(&mut self, rows: Vec<EnvRow>) {
        self.env_rows = rows;
        self.env_scroll = self.env_scroll.min(self.env_max_scroll());
    }

    fn env_viewport(&self) -> Rect {
        let c = self.content_rect(PanelId::Environment);
        Rectangle::new(
            (c.loc.x + PAD, c.loc.y + PAD).into(),
            (c.size.w - 2 * PAD, c.size.h - 2 * PAD).into(),
        )
    }

    fn env_visible_rows(&self) -> usize {
        (self.env_viewport().size.h / ENV_ROW_H).max(1) as usize
    }

    fn env_max_scroll(&self) -> usize {
        self.env_rows.len().saturating_sub(self.env_visible_rows())
    }

    /// Scroll the Environment list by `amount` (pointer axis units; positive
    /// scrolls down).
    pub fn scroll_environment(&mut self, amount: f64) {
        self.env_scroll_acc += amount;
        let rows = (self.env_scroll_acc / ENV_ROW_H as f64).trunc();
        if rows != 0.0 {
            self.env_scroll_acc -= rows * ENV_ROW_H as f64;
            let next = (self.env_scroll as i64 + rows as i64).clamp(0, self.env_max_scroll() as i64);
            self.env_scroll = next as usize;
        }
    }

    fn render_environment(&mut self, text: &mut TextRenderer, renderer: &mut GlesRenderer, elems: &mut Vec<CustomElement>) {
        let vp = self.env_viewport();
        let visible = self.env_visible_rows();
        let total = self.env_rows.len();
        let scrollable = total > visible;
        let text_w = vp.size.w - if scrollable { SCROLLBAR_W + 8 } else { 0 };
        let value_x = vp.loc.x + ENV_KEY_W + 12;
        let value_w = vp.loc.x + text_w - value_x;
        let max_scroll = self.env_max_scroll().max(1) as f64;

        // Buffers: one header background per visible row, then track + thumb.
        let s = bufs(&mut self.environment.solids, visible + 2);
        let mut backs = Vec::new();
        for (slot, row) in self.env_rows.iter().skip(self.env_scroll).take(visible).enumerate() {
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
            let thumb_y = track.loc.y + ((self.env_scroll as f64 / max_scroll) * (track.size.h - thumb_h) as f64) as i32;
            let thumb = Rectangle::new((track.loc.x, thumb_y).into(), (SCROLLBAR_W, thumb_h).into());
            elems.push(ui::solid(&mut s[visible + 1], thumb, SCROLL_THUMB));
            elems.push(ui::solid(&mut s[visible], track, SCROLL_TRACK));
        }
    }

    fn render_launcher(
        &mut self,
        text: &mut TextRenderer,
        renderer: &mut GlesRenderer,
        hovered: Option<PanelHit>,
        elems: &mut Vec<CustomElement>,
    ) {
        let l = self.launcher_layout();
        let n = self.entries.len();
        // Buffers: [field, remove] per entry, then caret, ring, add.
        let s = bufs(&mut self.launcher.solids, 2 * n + 3);
        let (caret_i, ring_i, add_i) = (2 * n, 2 * n + 1, 2 * n + 2);

        elems.extend(ui::text_in(
            text,
            renderer,
            "Executables shown in the Launcher menu (absolute paths)",
            l.header.loc.x,
            l.header,
        ));
        for (i, (field, remove)) in l.fields.iter().enumerate() {
            let entry = &self.entries[i];
            let editing = self.editing == Some(i);
            let tx = field.loc.x + FIELD_TEXT_PAD;
            if entry.is_empty() && !editing {
                elems.extend(ui::text_in_alpha(text, renderer, "/path/to/executable", tx, *field, 0.35));
            } else {
                let cursor = editing.then_some(self.cursor);
                let view = field_view(text, entry, cursor, field.size.w - 2 * FIELD_TEXT_PAD);
                elems.extend(ui::text_in(text, renderer, &view.visible, tx, *field));
                if let Some(cx) = view.caret_x {
                    let caret = Rectangle::new((tx + cx, field.loc.y + 4).into(), (1, field.size.h - 8).into());
                    elems.push(ui::solid(&mut s[caret_i], caret, CARET));
                }
            }
            elems.extend(ui::text_centered(text, renderer, "Remove", *remove));
        }
        elems.extend(ui::text_centered(text, renderer, "Add", l.add));

        for (i, (field, remove)) in l.fields.iter().enumerate() {
            let color = if self.editing == Some(i) { FIELD_BG_EDIT } else { FIELD_BG };
            elems.push(ui::solid(&mut s[2 * i], *field, color));
            let rc = if hovered == Some(PanelHit::Remove(i)) { BUTTON_HOT } else { BUTTON };
            elems.push(ui::solid(&mut s[2 * i + 1], *remove, rc));
        }
        if let Some((field, _)) = self.editing.and_then(|i| l.fields.get(i)) {
            let mut ring = *field;
            ring.loc -= (1, 1).into();
            ring.size += (2, 2).into();
            elems.push(ui::solid(&mut s[ring_i], ring, FIELD_EDIT_RING));
        }
        let ac = if hovered == Some(PanelHit::Add) { BUTTON_HOT } else { BUTTON };
        elems.push(ui::solid(&mut s[add_i], l.add, ac));
    }
}

/// What part of a field's text is visible.
struct FieldView {
    /// Index (in chars) of the first visible char.
    start: usize,
    visible: String,
    /// Caret x offset from the text start, when editing.
    caret_x: Option<i32>,
}

fn field_view(text: &TextRenderer, s: &str, cursor: Option<usize>, width: i32) -> FieldView {
    let chars: Vec<char> = s.chars().collect();
    let substr = |a: usize, b: usize| chars[a..b].iter().collect::<String>();

    if text.measure(s) <= width {
        return FieldView {
            start: 0,
            visible: s.to_string(),
            caret_x: cursor.map(|c| text.measure(&substr(0, c))),
        };
    }
    match cursor {
        // Scroll so the caret is visible.
        Some(c) => {
            let mut start = 0;
            while start < c && text.measure(&substr(start, c)) > width {
                start += 1;
            }
            let mut end = c;
            while end < chars.len() && text.measure(&substr(start, end + 1)) <= width {
                end += 1;
            }
            FieldView {
                start,
                visible: substr(start, end),
                caret_x: Some(text.measure(&substr(start, c))),
            }
        }
        None => FieldView {
            start: 0,
            visible: text.fit(s, width),
            caret_x: None,
        },
    }
}

fn byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len())
}

fn insert_char(s: &mut String, char_idx: usize, c: char) {
    let b = byte_index(s, char_idx);
    s.insert(b, c);
}

fn remove_char(s: &mut String, char_idx: usize) {
    let b = byte_index(s, char_idx);
    if b < s.len() {
        s.remove(b);
    }
}

/// Display name for a launcher entry: the executable's file name.
pub fn exe_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

// --- database ---------------------------------------------------------------

pub fn db_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/simplewm/simplewm.db"))
}

fn open_db() -> Option<Connection> {
    let path = db_path()?;
    if let Some(dir) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(dir) {
            tracing::error!(?err, ?dir, "failed to create config directory");
            return None;
        }
    }
    let conn = match Connection::open(&path) {
        Ok(c) => c,
        Err(err) => {
            tracing::error!(?err, ?path, "failed to open database");
            return None;
        }
    };
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap_or(0);
    if version < 1 {
        // First run: create the schema and the default launcher entry. After
        // this the entry can be deleted and stays deleted.
        let init = format!(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS launcher (position INTEGER PRIMARY KEY, path TEXT NOT NULL);
             INSERT INTO launcher (position, path) VALUES (0, '{DEFAULT_LAUNCHER_ENTRY}');
             PRAGMA user_version = 1;
             COMMIT;"
        );
        if let Err(err) = conn.execute_batch(&init) {
            tracing::error!(?err, "failed to initialize database");
            return None;
        }
    }
    tracing::info!(?path, "opened database");
    Some(conn)
}

fn load_entries(conn: &Connection) -> Vec<String> {
    let result = (|| -> rusqlite::Result<Vec<String>> {
        let mut stmt = conn.prepare("SELECT path FROM launcher ORDER BY position")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect()
    })();
    result.unwrap_or_else(|err| {
        tracing::error!(?err, "failed to load launcher entries");
        Vec::new()
    })
}
