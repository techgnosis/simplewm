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
//! * The Sound applet window chooses PipeWire's sound card and sets its volume
//!   (see `sound` and `crate::sound`).
//! * The Password window is not in the Control Panel either: it asks for the
//!   user's password when an applet needs root, and runs that through sudo
//!   (see `sudo`).
//! * The File Picker window is not in the Control Panel: it opens when an app
//!   asks for a file through the desktop portal (see `picker`), or from the
//!   Launcher and Screenshot applets.
//!
//! Panels stack, focus and move like client windows. Settings are stored in
//! SQLite at ~/.config/simplewm/simplewm.db (see `db`).
//!
//! This file has what all panels share: window placement, the frame, hit
//! testing and rendering dispatch, shared sizes and colors. Each window's
//! layout, hit testing, actions and drawing live in its own submodule
//! (`control` also has Help; `rows` has Environment and Desktop Portal), as
//! `impl Panels` blocks. Editable text fields are in `text_field`.

use rusqlite::Connection;
use smithay::{
    backend::renderer::{element::solid::SolidColorBuffer, gles::GlesRenderer},
    utils::{Logical, Point, Rectangle},
};

use crate::{
    text::TextRenderer,
    ui::{self, CustomElement, Frame},
};

mod control;
mod db;
mod launcher;
mod network;
mod picker;
mod rows;
mod screenshot;
mod sound;
mod sudo;
mod text_field;

pub use db::db_path;
pub use launcher::exe_name;
pub use network::NetworkApplet;
pub use picker::{FilePicker, PickHit};
pub use rows::EnvRow;
pub use sound::SoundApplet;
pub use sudo::{SudoDone, SudoPrompt};

use rows::{ENV_H, ENV_ROW_H, ENV_W, PORTAL_H, PORTAL_W, RowList};
use text_field::{field_char_at, render_field};

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
    Sound,
    FilePicker,
    Password,
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
            PanelId::Sound => "Sound",
            PanelId::FilePicker => "File Picker",
            PanelId::Password => "Password",
        }
    }
}

/// Applets shown as icons in the Control Panel: (name, window it opens).
pub const APPLETS: [(&str, PanelId); 7] = [
    ("Launcher", PanelId::Launcher),
    ("Environment", PanelId::Environment),
    ("Screenshot", PanelId::Screenshot),
    ("Desktop Portal", PanelId::Portal),
    ("Network", PanelId::Network),
    ("Sound", PanelId::Sound),
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
    /// The user's password, in the Password window (shown as dots).
    SudoPassword,
}

impl Field {
    pub fn panel(self) -> PanelId {
        match self {
            Field::PickName => PanelId::FilePicker,
            Field::NetAddress | Field::NetGateway => PanelId::Network,
            Field::SudoPassword => PanelId::Password,
        }
    }

    /// The field Tab moves to.
    fn next(self) -> Option<Field> {
        match self {
            Field::NetAddress => Some(Field::NetGateway),
            Field::NetGateway => Some(Field::NetAddress),
            Field::PickName | Field::SudoPassword => None,
        }
    }
}

// --- Layout shared by the panels (logical pixels) ----------------------------

const PAD: i32 = 14;
const BTN_H: i32 = 26;
const HEADER_H: i32 = 26;
const FIELD_H: i32 = 26;
const FIELD_TEXT_PAD: i32 = 6;
const NOTE_H: i32 = 24;
const SCROLLBAR_W: i32 = 6;

// --- Colors shared by the panels (premultiplied RGBA) -------------------------

const BG: [f32; 4] = [0.13, 0.14, 0.17, 1.0];
const FIELD_BG: [f32; 4] = [0.08, 0.09, 0.11, 1.0];
const FIELD_BG_EDIT: [f32; 4] = [0.05, 0.06, 0.08, 1.0];
const FIELD_EDIT_RING: [f32; 4] = [0.25, 0.42, 0.70, 1.0];
const BUTTON: [f32; 4] = [0.22, 0.24, 0.29, 1.0];
const BUTTON_HOT: [f32; 4] = [0.32, 0.35, 0.42, 1.0];
const SEPARATOR: [f32; 4] = [0.25, 0.27, 0.31, 1.0];
const CARET: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const CELL_HOT: [f32; 4] = [0.20, 0.24, 0.32, 1.0];
const SCROLL_TRACK: [f32; 4] = [0.18, 0.19, 0.23, 1.0];
const SCROLL_THUMB: [f32; 4] = [0.40, 0.43, 0.50, 1.0];

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
    // Sound: a card, the volume bar (percent under the pointer), − and +.
    SoundCard(usize),
    SoundBar(u32),
    SoundDown,
    SoundUp,
    // Password window
    SudoOk,
    SudoCancel,
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

pub struct Panels {
    control: PanelWindow,
    launcher: PanelWindow,
    environment: PanelWindow,
    screenshot: PanelWindow,
    help: PanelWindow,
    portal: PanelWindow,
    network: PanelWindow,
    sound_window: PanelWindow,
    file_picker: PanelWindow,
    password: PanelWindow,
    pub picker: FilePicker,
    pub sudo: SudoPrompt,
    /// A panel that asked to be opened and focused (see `take_open_request`).
    open_request: Option<PanelId>,
    pub net: NetworkApplet,
    sound: SoundApplet,
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
        let db = db::open_db();
        let entries = match &db {
            Some(conn) => db::load_entries(conn),
            None => vec![DEFAULT_LAUNCHER_ENTRY.to_string()],
        };
        let shot_dir = db
            .as_ref()
            .and_then(|c| db::load_setting(c, screenshot::SHOT_DIR_KEY))
            .unwrap_or_default();
        let mut panels = Self {
            control: PanelWindow::default(),
            launcher: PanelWindow::default(),
            environment: PanelWindow::default(),
            screenshot: PanelWindow::default(),
            help: PanelWindow::default(),
            portal: PanelWindow::default(),
            network: PanelWindow::default(),
            sound_window: PanelWindow::default(),
            file_picker: PanelWindow::default(),
            password: PanelWindow::default(),
            picker: FilePicker::default(),
            sudo: SudoPrompt::default(),
            open_request: None,
            net: NetworkApplet::default(),
            sound: SoundApplet::default(),
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
            PanelId::Sound => &self.sound_window,
            PanelId::FilePicker => &self.file_picker,
            PanelId::Password => &self.password,
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
            PanelId::Sound => &mut self.sound_window,
            PanelId::FilePicker => &mut self.file_picker,
            PanelId::Password => &mut self.password,
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

    // --- geometry -------------------------------------------------------------

    fn content_size(&self, id: PanelId) -> (i32, i32) {
        match id {
            PanelId::ControlPanel => self.control_size(),
            PanelId::Launcher => self.launcher_size(),
            PanelId::Environment => (ENV_W, ENV_H),
            PanelId::Screenshot => self.screenshot_size(),
            PanelId::Help => self.help_size(),
            PanelId::Portal => (PORTAL_W, PORTAL_H),
            PanelId::Network => self.network_size(),
            PanelId::Sound => self.sound_size(),
            PanelId::FilePicker => self.picker_size(),
            PanelId::Password => self.sudo_size(),
        }
    }

    pub fn content_rect(&self, id: PanelId) -> Rect {
        Rectangle::new(
            self.win(id).loc.unwrap_or_default(),
            self.content_size(id).into(),
        )
    }

    fn frame(&self, id: PanelId, text: &TextRenderer) -> Frame {
        Frame::new(self.content_rect(id), text)
    }

    /// Open a panel. The first time, the Control Panel is centered and applet
    /// windows open offset from it. The File Picker is always centered.
    pub fn show(&mut self, id: PanelId, out: Rect) {
        if self.win(id).loc.is_none() {
            let (w, h) = self.content_size(id);
            let min_y = out.loc.y + ui::BAR_HEIGHT + ui::TITLE_HEIGHT;
            let loc = match (id, self.control.loc) {
                (PanelId::ControlPanel | PanelId::FilePicker | PanelId::Password, _)
                | (_, None) => Point::from((
                    out.loc.x + (out.size.w - w) / 2,
                    (out.loc.y + (out.size.h - h) / 3).max(min_y),
                )),
                (_, Some(cp)) => cp + Point::from((40, 40)),
            };
            self.win_mut(id).loc = Some(loc);
        }
        self.win_mut(id).open = true;
    }

    /// A panel an action asked to open (the Password window), for `State` to
    /// show and focus.
    pub fn take_open_request(&mut self) -> Option<PanelId> {
        self.open_request.take()
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

    pub fn hit(
        &self,
        id: PanelId,
        text: &TextRenderer,
        p: Point<f64, Logical>,
    ) -> Option<PanelHit> {
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
        let hit = match id {
            PanelId::ControlPanel => self.control_hit(p),
            PanelId::Launcher => self.launcher_hit(p),
            PanelId::Screenshot => self.screenshot_hit(p),
            PanelId::FilePicker => self.picker_hit(text, p),
            PanelId::Password => self.sudo_hit(text, p),
            PanelId::Portal => self.portal_hit(p),
            PanelId::Network => self.network_hit(text, p),
            PanelId::Sound => self.sound_hit(p),
            PanelId::Environment | PanelId::Help => None,
        };
        Some(hit.unwrap_or(PanelHit::Body))
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
            PanelId::Sound => self.render_sound(text, renderer, hovered, elems),
            PanelId::FilePicker => self.render_picker(text, renderer, hovered, elems),
            PanelId::Password => self.render_sudo(text, renderer, hovered, elems),
        }

        // Frame: content background, title bar with close button, borders.
        let f = self.frame(id, text);
        let active = self.active == Some(id);
        let title = text.fit(self.title(id), f.title_text_max);
        let fb = &mut self.win_mut(id).frame;
        elems.push(ui::solid(buf(&mut fb.bg), f.content, BG));
        elems.extend(ui::text_in(text, renderer, &title, f.title_text_x, f.title));
        elems.extend(ui::text_centered(text, renderer, "×", f.close));
        let title_color = if active {
            ui::TITLE_FOCUSED
        } else {
            ui::TITLE_UNFOCUSED
        };
        let cc = if hovered == Some(PanelHit::Close) {
            ui::CLOSE_HOT
        } else {
            ui::CLOSE_COLOR
        };
        elems.push(ui::solid(buf(&mut fb.close), f.close, cc));
        elems.push(ui::solid(buf(&mut fb.title), f.title, title_color));
        elems.push(ui::solid(buf(&mut fb.left), f.left, title_color));
        elems.push(ui::solid(buf(&mut fb.right), f.right, title_color));
        elems.push(ui::solid(buf(&mut fb.bottom), f.bottom, title_color));
    }
}
