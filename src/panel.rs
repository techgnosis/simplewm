//! Compositor-drawn settings windows ("panels"), Windows Control Panel style:
//!
//! * The Control Panel window shows one icon per settings applet, plus a
//!   Quit button. Clicking an icon opens that applet's own window.
//! * The Launcher applet window edits the list of executable paths shown in
//!   the Launcher menu; paths are chosen with the File Picker.
//! * The Environment applet window shows (read-only) how simplewm was started:
//!   Wayland socket, backend devices, and environment variables.
//! * The Screenshot applet window sets the folder screenshots are saved in,
//!   chosen with the File Picker.
//! * The Help applet window shows a few static notes about simplewm.
//! * The Desktop Portal applet window shows checks for when apps get no file
//!   dialog from the desktop portal (see `crate::portal_check`).
//! * The Network applet window sets a static IPv4 address and gateway on the
//!   wired network card (see `network` and `crate::network`).
//! * The File Picker window is not in the Control Panel: it opens when an app
//!   asks for a file through the desktop portal (see `picker`), or from the
//!   Launcher and Screenshot applets.
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

mod network;
mod picker;

pub use network::NetworkApplet;
pub use picker::{FilePicker, PickHit};

pub const DEFAULT_LAUNCHER_ENTRY: &str = "/usr/bin/alacritty";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelId {
    ControlPanel,
    Launcher,
    Environment,
    Screenshot,
    Help,
    Portal,
    Network,
    FilePicker,
}

impl PanelId {

    pub fn title(self) -> &'static str {
        match self {
            PanelId::ControlPanel => "Control Panel",
            PanelId::Launcher => "Launcher",
            PanelId::Environment => "Environment",
            PanelId::Screenshot => "Screenshot",
            PanelId::Help => "Help",
            PanelId::Portal => "Desktop Portal",
            PanelId::Network => "Network",
            PanelId::FilePicker => "File Picker",
        }
    }
}

/// Applets shown as icons in the Control Panel: (name, window it opens).
pub const APPLETS: [(&str, PanelId); 6] = [
    ("Launcher", PanelId::Launcher),
    ("Environment", PanelId::Environment),
    ("Screenshot", PanelId::Screenshot),
    ("Desktop Portal", PanelId::Portal),
    ("Network", PanelId::Network),
    ("Help", PanelId::Help),
];

/// An editable text field in one of the panels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// File name in the File Picker (Save).
    PickName,
    /// Address with prefix length in the Network applet.
    NetAddress,
    /// Default gateway in the Network applet.
    NetGateway,
}

impl Field {
    pub fn panel(self) -> PanelId {
        match self {
            Field::PickName => PanelId::FilePicker,
            Field::NetAddress | Field::NetGateway => PanelId::Network,
        }
    }

    /// The field Tab moves to.
    fn next(self) -> Option<Field> {
        match self {
            Field::NetAddress => Some(Field::NetGateway),
            Field::NetGateway => Some(Field::NetAddress),
            Field::PickName => None,
        }
    }
}

// --- Layout (logical pixels) -----------------------------------------------

const PAD: i32 = 14;
const BTN_H: i32 = 26;

// Control Panel
const CP_W: i32 = 640;
const CELL_W: i32 = 102;
const CELL_H: i32 = 92;
const TILE: i32 = 48;
const QUIT_W: i32 = 130;

// Launcher applet
const LAUNCHER_W: i32 = 520;
const HEADER_H: i32 = 26;
const ROW_H: i32 = 32;
const FIELD_H: i32 = 26;
const FIELD_TEXT_PAD: i32 = 6;
const CHANGE_W: i32 = 90;
const REMOVE_W: i32 = 76;
const ADD_W: i32 = 90;

// Environment applet
const ENV_W: i32 = 700;
const ENV_H: i32 = 520;
const ENV_ROW_H: i32 = 20;
const ENV_KEY_W: i32 = 230;
const SCROLLBAR_W: i32 = 6;

// Desktop Portal applet
const PORTAL_W: i32 = 760;
const PORTAL_H: i32 = 560;
const REFRESH_W: i32 = 100;

// Screenshot applet
const SHOT_W: i32 = 520;
const CHOOSE_W: i32 = 90;
const NOTE_H: i32 = 24;

// Help applet
const HELP_W: i32 = 460;
const HELP_ROW_H: i32 = 24;
const HELP_LINES: [&str; 4] = [
    "No flags or environment variables. Just run simplewm.",
    "The resolution is the monitor's preferred mode.",
    "There is no plain text config file.",
    "There are no keyboard shortcuts.",
];

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
const TILE_HELP: [f32; 4] = [0.80, 0.50, 0.15, 1.0];
const TILE_SCREENSHOT: [f32; 4] = [0.55, 0.32, 0.75, 1.0];
const TILE_PORTAL: [f32; 4] = [0.15, 0.55, 0.65, 1.0];
const TILE_NETWORK: [f32; 4] = [0.70, 0.30, 0.35, 1.0];
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
    // Text field `.0`, with the char index under the pointer.
    Field(Field, usize),
    Change(usize),
    Remove(usize),
    Add,
    // Screenshot
    ChooseShotDir,
    // Desktop Portal
    Refresh,
    // Network
    NetApply,
    NetRefresh,
    // File Picker
    Pick(PickHit),
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

struct ScreenshotLayout {
    header: Rect,
    /// The folder, shown read-only.
    path: Rect,
    choose: Rect,
    note: Rect,
}

struct LauncherLayout {
    header: Rect,
    rows: Vec<LauncherRow>,
    add: Rect,
}

/// One entry: its path (read-only), Change and Remove buttons.
struct LauncherRow {
    path: Rect,
    change: Rect,
    remove: Rect,
}

/// A row in the Environment or Desktop Portal window.
#[derive(Debug, Clone)]
pub enum EnvRow {
    Header(String),
    Item(String, String),
    /// Text across the whole row.
    Line(String),
}

/// A scrollable list of rows (Environment, Desktop Portal).
#[derive(Default)]
struct RowList {
    rows: Vec<EnvRow>,
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

    fn set(&mut self, rows: Vec<EnvRow>, vp: Rect) {
        self.rows = rows;
        self.scroll = self.scroll.min(self.max_scroll(vp));
    }

    /// Scroll by `amount` (pointer axis units; positive scrolls down).
    fn scroll_by(&mut self, amount: f64, vp: Rect) {
        self.scroll_acc += amount;
        let rows = (self.scroll_acc / ENV_ROW_H as f64).trunc();
        if rows != 0.0 {
            self.scroll_acc -= rows * ENV_ROW_H as f64;
            let next = (self.scroll as i64 + rows as i64).clamp(0, self.max_scroll(vp) as i64);
            self.scroll = next as usize;
        }
    }

    fn render(
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
            let thumb_y = track.loc.y + ((self.scroll as f64 / max_scroll) * (track.size.h - thumb_h) as f64) as i32;
            let thumb = Rectangle::new((track.loc.x, thumb_y).into(), (SCROLLBAR_W, thumb_h).into());
            elems.push(ui::solid(&mut s[visible + 1], thumb, SCROLL_THUMB));
            elems.push(ui::solid(&mut s[visible], track, SCROLL_TRACK));
        }
    }
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
        // A question mark: hook, stem and dot.
        PanelId::Help => {
            let (x, y) = (tile.loc.x, tile.loc.y);
            let r = |dx, dy, w, h| Rectangle::new((x + dx, y + dy).into(), (w, h).into());
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
            let (x, y) = (tile.loc.x, tile.loc.y);
            let r = |dx, dy, w, h| Rectangle::new((x + dx, y + dy).into(), (w, h).into());
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
            let (x, y) = (tile.loc.x, tile.loc.y);
            let r = |dx, dy, w, h| Rectangle::new((x + dx, y + dy).into(), (w, h).into());
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
            let (x, y) = (tile.loc.x, tile.loc.y);
            let r = |dx, dy, w, h| Rectangle::new((x + dx, y + dy).into(), (w, h).into());
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
        PanelId::ControlPanel | PanelId::FilePicker => (TILE_LAUNCHER, Vec::new()),
    }
}

pub struct Panels {
    control: PanelWindow,
    launcher: PanelWindow,
    environment: PanelWindow,
    screenshot: PanelWindow,
    help: PanelWindow,
    portal: PanelWindow,
    network: PanelWindow,
    file_picker: PanelWindow,
    pub picker: FilePicker,
    pub net: NetworkApplet,
    /// Environment rows (refreshed when the window is opened).
    env: RowList,
    /// Desktop Portal check results (refreshed on open and by Refresh).
    portal_rows: RowList,
    portal_refresh_buf: Option<SolidColorBuffer>,
    /// Panel with keyboard focus (instead of a client window).
    pub active: Option<PanelId>,

    // Launcher settings
    pub entries: Vec<String>,
    /// Screenshot folder (may be empty; older settings may start with ~).
    shot_dir: String,
    /// Field being edited and the caret position (in chars).
    pub editing: Option<Field>,
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
        let shot_dir = db.as_ref().and_then(|c| load_setting(c, "screenshot_dir")).unwrap_or_default();
        let mut panels = Self {
            control: PanelWindow::default(),
            launcher: PanelWindow::default(),
            environment: PanelWindow::default(),
            screenshot: PanelWindow::default(),
            help: PanelWindow::default(),
            portal: PanelWindow::default(),
            network: PanelWindow::default(),
            file_picker: PanelWindow::default(),
            picker: FilePicker::default(),
            net: NetworkApplet::default(),
            env: RowList::default(),
            portal_rows: RowList::default(),
            portal_refresh_buf: None,
            active: None,
            entries,
            shot_dir,
            editing: None,
            cursor: 0,
            db,
        };
        panels.load_network();
        panels
    }

    fn win(&self, id: PanelId) -> &PanelWindow {
        match id {
            PanelId::ControlPanel => &self.control,
            PanelId::Launcher => &self.launcher,
            PanelId::Environment => &self.environment,
            PanelId::Screenshot => &self.screenshot,
            PanelId::Help => &self.help,
            PanelId::Portal => &self.portal,
            PanelId::Network => &self.network,
            PanelId::FilePicker => &self.file_picker,
        }
    }

    fn win_mut(&mut self, id: PanelId) -> &mut PanelWindow {
        match id {
            PanelId::ControlPanel => &mut self.control,
            PanelId::Launcher => &mut self.launcher,
            PanelId::Environment => &mut self.environment,
            PanelId::Screenshot => &mut self.screenshot,
            PanelId::Help => &mut self.help,
            PanelId::Portal => &mut self.portal,
            PanelId::Network => &mut self.network,
            PanelId::FilePicker => &mut self.file_picker,
        }
    }

    pub fn is_open(&self, id: PanelId) -> bool {
        self.win(id).open
    }

    pub fn loc(&self, id: PanelId) -> Option<Point<i32, Logical>> {
        self.win(id).loc
    }

    pub fn set_loc(&mut self, id: PanelId, loc: Point<i32, Logical>) {
        self.win_mut(id).loc = Some(loc);
    }

    /// Forget where a panel was, so the next `show` places it afresh.
    pub fn forget_loc(&mut self, id: PanelId) {
        self.win_mut(id).loc = None;
    }

    /// Title bar text.
    fn title(&self, id: PanelId) -> &str {
        match id {
            PanelId::FilePicker => &self.picker.title,
            _ => id.title(),
        }
    }

    /// Non-empty launcher paths, in order.
    pub fn launcher_paths(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty())
            .collect()
    }

    /// Folder to save screenshots in, if one is set (`~` is expanded).
    pub fn screenshot_dir(&self) -> Option<PathBuf> {
        let dir = self.shot_dir.trim();
        if dir.is_empty() {
            return None;
        }
        let home = || std::env::var_os("HOME").map(PathBuf::from);
        if dir == "~" {
            return home();
        }
        if let Some(rest) = dir.strip_prefix("~/") {
            return home().map(|h| h.join(rest));
        }
        Some(PathBuf::from(dir))
    }

    /// Set the screenshot folder (chosen in the File Picker) and save it.
    pub fn set_screenshot_dir(&mut self, dir: &Path) {
        self.shot_dir = dir.display().to_string();
        self.save();
    }

    fn field_text(&self, f: Field) -> &String {
        match f {
            Field::PickName => &self.picker.name,
            Field::NetAddress => &self.net.address,
            Field::NetGateway => &self.net.gateway,
        }
    }

    fn field_text_mut(&mut self, f: Field) -> &mut String {
        match f {
            Field::PickName => &mut self.picker.name,
            Field::NetAddress => &mut self.net.address,
            Field::NetGateway => &mut self.net.gateway,
        }
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
            PanelId::Screenshot => (SHOT_W, PAD + HEADER_H + FIELD_H + NOTE_H + PAD),
            PanelId::Help => (HELP_W, PAD + HELP_LINES.len() as i32 * HELP_ROW_H + PAD),
            PanelId::Portal => (PORTAL_W, PORTAL_H),
            PanelId::Network => self.network_size(),
            PanelId::FilePicker => self.picker_size(),
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
        let path_w = inner_w - CHANGE_W - REMOVE_W - 16;
        let mut rows = Vec::new();
        for _ in &self.entries {
            let change_x = x + path_w + 8;
            rows.push(LauncherRow {
                path: Rectangle::new((x, y).into(), (path_w, FIELD_H).into()),
                change: Rectangle::new((change_x, y).into(), (CHANGE_W, FIELD_H).into()),
                remove: Rectangle::new((change_x + CHANGE_W + 8, y).into(), (REMOVE_W, FIELD_H).into()),
            });
            y += ROW_H;
        }
        let add = Rectangle::new((x, y + 4).into(), (ADD_W, BTN_H).into());
        LauncherLayout { header, rows, add }
    }

    fn screenshot_layout(&self) -> ScreenshotLayout {
        let c = self.content_rect(PanelId::Screenshot);
        let x = c.loc.x + PAD;
        let w = c.size.w - 2 * PAD;
        let y = c.loc.y + PAD;
        let path_w = w - CHOOSE_W - 8;
        ScreenshotLayout {
            header: Rectangle::new((x, y).into(), (w, HEADER_H).into()),
            path: Rectangle::new((x, y + HEADER_H).into(), (path_w, FIELD_H).into()),
            choose: Rectangle::new((x + path_w + 8, y + HEADER_H).into(), (CHOOSE_W, FIELD_H).into()),
            note: Rectangle::new((x, y + HEADER_H + FIELD_H).into(), (w, NOTE_H).into()),
        }
    }

    /// Open a panel. The first time, the Control Panel is centered and applet
    /// windows open offset from it. The File Picker is always centered.
    pub fn show(&mut self, id: PanelId, out: Rect) {
        if self.win(id).loc.is_none() {
            let (w, h) = self.content_size(id);
            let min_y = out.loc.y + ui::BAR_HEIGHT + ui::TITLE_HEIGHT;
            let loc = match (id, self.control.loc) {
                (PanelId::ControlPanel | PanelId::FilePicker, _) | (_, None) => Point::from((
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
        if self.editing.map(Field::panel) == Some(id) {
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
                for (i, row) in l.rows.iter().enumerate() {
                    if row.change.to_f64().contains(p) {
                        return Some(PanelHit::Change(i));
                    }
                    if row.remove.to_f64().contains(p) {
                        return Some(PanelHit::Remove(i));
                    }
                }
                if l.add.to_f64().contains(p) {
                    return Some(PanelHit::Add);
                }
            }
            PanelId::Screenshot => {
                if self.screenshot_layout().choose.to_f64().contains(p) {
                    return Some(PanelHit::ChooseShotDir);
                }
            }
            PanelId::FilePicker => {
                if let Some(h) = self.picker_hit(text, p) {
                    return Some(h);
                }
            }
            PanelId::Portal => {
                if self.portal_refresh().to_f64().contains(p) {
                    return Some(PanelHit::Refresh);
                }
            }
            PanelId::Network => {
                if let Some(h) = self.network_hit(text, p) {
                    return Some(h);
                }
            }
            PanelId::Environment | PanelId::Help => {}
        }
        Some(PanelHit::Body)
    }

    // --- Text field editing ---------------------------------------------------

    pub fn begin_edit(&mut self, field: Field, cursor: usize) {
        self.editing = Some(field);
        self.cursor = cursor.min(self.field_text(field).chars().count());
    }

    pub fn stop_editing(&mut self) {
        self.editing = None;
    }

    // --- Launcher entries -------------------------------------------------------

    /// Add executables chosen in the File Picker (skipping ones already listed).
    pub fn add_entries(&mut self, paths: &[PathBuf]) {
        for p in paths {
            let p = p.display().to_string();
            if !self.entries.contains(&p) {
                self.entries.push(p);
            }
        }
        self.save();
    }

    /// Replace entry `old` with `new` (appended if `old` was removed meanwhile).
    pub fn change_entry(&mut self, old: &str, new: &Path) {
        let new = new.display().to_string();
        match self.entries.iter().position(|e| e == old) {
            Some(i) => self.entries[i] = new,
            None => self.entries.push(new),
        }
        self.save();
    }

    pub fn remove_entry(&mut self, i: usize) {
        if i >= self.entries.len() {
            return;
        }
        self.entries.remove(i);
        self.save();
    }

    /// Handle a key press while a text field is being edited.
    pub fn key(&mut self, sym: u32, ch: Option<char>, ctrl: bool) {
        let Some(f) = self.editing else {
            return;
        };
        let len = self.field_text(f).chars().count();
        match sym {
            keysyms::KEY_BackSpace => {
                if self.cursor > 0 {
                    let c = self.cursor - 1;
                    remove_char(self.field_text_mut(f), c);
                    self.cursor -= 1;
                }
            }
            keysyms::KEY_Delete | keysyms::KEY_KP_Delete => {
                if self.cursor < len {
                    let c = self.cursor;
                    remove_char(self.field_text_mut(f), c);
                }
            }
            keysyms::KEY_Left | keysyms::KEY_KP_Left => self.cursor = self.cursor.saturating_sub(1),
            keysyms::KEY_Right | keysyms::KEY_KP_Right => self.cursor = (self.cursor + 1).min(len),
            keysyms::KEY_Home | keysyms::KEY_KP_Home => self.cursor = 0,
            keysyms::KEY_End | keysyms::KEY_KP_End => self.cursor = len,
            keysyms::KEY_Tab | keysyms::KEY_ISO_Left_Tab => {
                if let Some(next) = f.next() {
                    let end = self.field_text(next).chars().count();
                    self.begin_edit(next, end);
                }
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter if f.panel() == PanelId::Network => self.network_apply(),
            keysyms::KEY_Return | keysyms::KEY_KP_Enter | keysyms::KEY_Escape => self.stop_editing(),
            _ => {
                if let Some(c) = ch
                    && !ctrl
                    && !c.is_control()
                {
                    let cursor = self.cursor;
                    insert_char(self.field_text_mut(f), cursor, c);
                    self.cursor += 1;
                }
            }
        }
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
        let shot_dir = self.shot_dir.trim();
        let result = (|| -> rusqlite::Result<()> {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM launcher", [])?;
            for (i, p) in paths.iter().enumerate() {
                tx.execute("INSERT INTO launcher (position, path) VALUES (?1, ?2)", (i as i64, p))?;
            }
            tx.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES ('screenshot_dir', ?1)",
                [shot_dir],
            )?;
            tx.commit()
        })();
        if let Err(err) = result {
            tracing::error!(?err, "failed to save settings");
        }
    }

    /// Store `(key, value)` pairs in the settings table.
    fn save_settings(&mut self, pairs: &[(&str, &str)]) {
        let Some(conn) = self.db.as_mut() else {
            return;
        };
        let result = (|| -> rusqlite::Result<()> {
            let tx = conn.transaction()?;
            for (key, value) in pairs {
                tx.execute("INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)", [key, value])?;
            }
            tx.commit()
        })();
        if let Err(err) = result {
            tracing::error!(?err, "failed to save settings");
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
            PanelId::Screenshot => self.render_screenshot(text, renderer, hovered, elems),
            PanelId::Help => self.render_help(text, renderer, elems),
            PanelId::Portal => self.render_portal(text, renderer, hovered, elems),
            PanelId::Network => self.render_network(text, renderer, hovered, elems),
            PanelId::FilePicker => self.render_picker(text, renderer, hovered, elems),
        }

        // Frame: content background, title bar with close button, borders.
        let f = self.frame(id, text);
        let active = self.active == Some(id);
        let title = text.fit(self.title(id), f.title_text_max);
        let fb = &mut self.win_mut(id).frame;
        elems.push(ui::solid(buf(&mut fb.bg), f.content, BG));
        elems.extend(ui::text_in(text, renderer, &title, f.title_text_x, f.title));
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

    /// Scroll a row list window (Environment, Desktop Portal) by `amount`
    /// (pointer axis units; positive scrolls down).
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

    fn render_environment(&mut self, text: &mut TextRenderer, renderer: &mut GlesRenderer, elems: &mut Vec<CustomElement>) {
        let vp = self.env_viewport();
        self.env.render(vp, &mut self.environment.solids, text, renderer, elems);
    }

    fn render_portal(
        &mut self,
        text: &mut TextRenderer,
        renderer: &mut GlesRenderer,
        hovered: Option<PanelHit>,
        elems: &mut Vec<CustomElement>,
    ) {
        let vp = self.portal_viewport();
        let refresh = self.portal_refresh();
        self.portal_rows.render(vp, &mut self.portal.solids, text, renderer, elems);
        elems.extend(ui::text_centered(text, renderer, "Refresh", refresh));
        let rc = if hovered == Some(PanelHit::Refresh) { BUTTON_HOT } else { BUTTON };
        elems.push(ui::solid(buf(&mut self.portal_refresh_buf), refresh, rc));
    }

    fn render_help(&self, text: &mut TextRenderer, renderer: &mut GlesRenderer, elems: &mut Vec<CustomElement>) {
        let c = self.content_rect(PanelId::Help);
        for (i, line) in HELP_LINES.iter().enumerate() {
            let r = Rectangle::new(
                (c.loc.x + PAD, c.loc.y + PAD + i as i32 * HELP_ROW_H).into(),
                (c.size.w - 2 * PAD, HELP_ROW_H).into(),
            );
            elems.extend(ui::text_in(text, renderer, line, r.loc.x, r));
        }
    }

    fn render_screenshot(
        &mut self,
        text: &mut TextRenderer,
        renderer: &mut GlesRenderer,
        hovered: Option<PanelHit>,
        elems: &mut Vec<CustomElement>,
    ) {
        let l = self.screenshot_layout();
        // Buffers: path box, Choose button.
        let s = bufs(&mut self.screenshot.solids, 2);

        elems.extend(ui::text_in(text, renderer, "Folder where screenshots are saved", l.header.loc.x, l.header));
        let tx = l.path.loc.x + FIELD_TEXT_PAD;
        let max = l.path.size.w - 2 * FIELD_TEXT_PAD;
        if self.shot_dir.trim().is_empty() {
            elems.extend(ui::text_in_alpha(text, renderer, "No folder chosen", tx, l.path, 0.35));
        } else {
            let shown = text.fit(&self.shot_dir, max);
            elems.extend(ui::text_in(text, renderer, &shown, tx, l.path));
        }
        elems.extend(ui::text_centered(text, renderer, "Choose…", l.choose));
        let note = "Files are named simplewm-screenshot-<date>-<time>.png";
        elems.extend(ui::text_in_alpha(text, renderer, note, l.note.loc.x, l.note, 0.6));

        elems.push(ui::solid(&mut s[0], l.path, FIELD_BG));
        let cc = if hovered == Some(PanelHit::ChooseShotDir) { BUTTON_HOT } else { BUTTON };
        elems.push(ui::solid(&mut s[1], l.choose, cc));
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
        // Buffers: [path, change, remove] per entry, then add.
        let s = bufs(&mut self.launcher.solids, 3 * n + 1);

        elems.extend(ui::text_in(text, renderer, "Executables shown in the Launcher menu", l.header.loc.x, l.header));
        for (i, row) in l.rows.iter().enumerate() {
            let tx = row.path.loc.x + FIELD_TEXT_PAD;
            let shown = text.fit(&self.entries[i], row.path.size.w - 2 * FIELD_TEXT_PAD);
            elems.extend(ui::text_in(text, renderer, &shown, tx, row.path));
            elems.extend(ui::text_centered(text, renderer, "Change…", row.change));
            elems.extend(ui::text_centered(text, renderer, "Remove", row.remove));
        }
        elems.extend(ui::text_centered(text, renderer, "Add…", l.add));

        let button = |hit: PanelHit| if hovered == Some(hit) { BUTTON_HOT } else { BUTTON };
        for (i, row) in l.rows.iter().enumerate() {
            elems.push(ui::solid(&mut s[3 * i], row.path, FIELD_BG));
            elems.push(ui::solid(&mut s[3 * i + 1], row.change, button(PanelHit::Change(i))));
            elems.push(ui::solid(&mut s[3 * i + 2], row.remove, button(PanelHit::Remove(i))));
        }
        elems.push(ui::solid(&mut s[3 * n], l.add, button(PanelHit::Add)));
    }
}

/// Focus ring just outside a field being edited.
fn edit_ring(field: Rect) -> Rect {
    Rectangle::new(field.loc - Point::from((1, 1)), field.size + (2, 2).into())
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

/// Char index in field `rect` (showing `value`) nearest to pointer x `x`.
fn field_char_at(text: &TextRenderer, value: &str, cursor: Option<usize>, rect: Rect, x: f64) -> usize {
    let view = field_view(text, value, cursor, rect.size.w - 2 * FIELD_TEXT_PAD);
    let mut pos = (rect.loc.x + FIELD_TEXT_PAD) as f64;
    let mut idx = view.start;
    for ch in value.chars().skip(view.start) {
        let adv = text.measure(&ch.to_string()) as f64;
        if x < pos + adv / 2.0 {
            break;
        }
        pos += adv;
        idx += 1;
    }
    idx
}

/// Draw an editable text field: its text (or a dimmed `placeholder` when
/// empty), the caret and focus ring when `cursor` is set. Uses 3 buffers.
#[allow(clippy::too_many_arguments)]
fn render_field(
    text: &mut TextRenderer,
    renderer: &mut GlesRenderer,
    s: &mut [SolidColorBuffer],
    rect: Rect,
    value: &str,
    placeholder: &str,
    cursor: Option<usize>,
    elems: &mut Vec<CustomElement>,
) {
    let tx = rect.loc.x + FIELD_TEXT_PAD;
    let view = field_view(text, value, cursor, rect.size.w - 2 * FIELD_TEXT_PAD);
    if value.is_empty() && !placeholder.is_empty() {
        elems.extend(ui::text_in_alpha(text, renderer, placeholder, tx, rect, 0.35));
    } else {
        elems.extend(ui::text_in(text, renderer, &view.visible, tx, rect));
    }
    if let Some(cx) = view.caret_x {
        let caret = Rectangle::new((tx + cx, rect.loc.y + 4).into(), (1, rect.size.h - 8).into());
        elems.push(ui::solid(&mut s[0], caret, CARET));
    }
    let editing = cursor.is_some();
    elems.push(ui::solid(&mut s[1], rect, if editing { FIELD_BG_EDIT } else { FIELD_BG }));
    if editing {
        elems.push(ui::solid(&mut s[2], edit_ring(rect), FIELD_EDIT_RING));
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
    if version < 2 {
        let migrate = "BEGIN;
             CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             PRAGMA user_version = 2;
             COMMIT;";
        if let Err(err) = conn.execute_batch(migrate) {
            tracing::error!(?err, "failed to migrate database");
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

fn load_setting(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
        .map_err(|err| {
            if !matches!(err, rusqlite::Error::QueryReturnedNoRows) {
                tracing::error!(?err, key, "failed to load setting");
            }
        })
        .ok()
}
