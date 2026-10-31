//! The File Picker applet: the dialog shown for portal file chooser requests
//! (see `crate::portal`). It is not listed in the Control Panel; it opens only
//! when an app asks for a file, and closing it answers the request.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use smithay::{
    backend::renderer::{element::solid::SolidColorBuffer, gles::GlesRenderer},
    input::keyboard::keysyms,
    utils::{Logical, Point, Rectangle},
};

use super::{
    BUTTON, BUTTON_HOT, CARET, FIELD_BG, FIELD_BG_EDIT, FIELD_EDIT_RING, FIELD_H, FIELD_TEXT_PAD, Field,
    NOTE_H, PAD, PanelHit, PanelId, Rect, SCROLL_THUMB, SCROLL_TRACK, SCROLLBAR_W, bufs, edit_ring, field_view,
};
use crate::{
    portal::{PickReply, PickRequest, PickResult, PortalEvent},
    state::State,
    text::TextRenderer,
    ui::{self, CustomElement},
};

// --- Layout (logical pixels) -----------------------------------------------

const PICK_W: i32 = 640;
const LIST_H: i32 = 320;
const ROW_H: i32 = 22;
const LIST_INSET: i32 = 2;
const NAV_W: i32 = 64;
const ACTION_W: i32 = 96;
const HIDDEN_W: i32 = 150;
const FILTER_W: i32 = 220;
const NAME_LABEL_W: i32 = 54;
const GAP: i32 = 8;
const BTN_H: i32 = super::BTN_H;

const DOUBLE_CLICK: Duration = Duration::from_millis(400);

const ROW_SELECTED: [f32; 4] = [0.25, 0.42, 0.70, 1.0];
const ROW_HOT: [f32; 4] = [0.18, 0.21, 0.27, 1.0];
const ACCEPT: [f32; 4] = [0.20, 0.36, 0.62, 1.0];
const ACCEPT_HOT: [f32; 4] = [0.27, 0.46, 0.78, 1.0];

/// A click target inside the File Picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickHit {
    Up,
    Home,
    /// Entry `.0` in the listing.
    Row(usize),
    Hidden,
    Filter,
    Cancel,
    Accept,
}

#[derive(Debug, Clone)]
struct Entry {
    name: String,
    is_dir: bool,
}

struct PickLayout {
    up: Rect,
    home: Rect,
    path: Rect,
    list: Rect,
    name_label: Rect,
    name: Option<Rect>,
    note: Rect,
    hidden: Rect,
    filter: Option<Rect>,
    cancel: Rect,
    accept: Rect,
}

#[derive(Default)]
pub struct FilePicker {
    /// The request being answered (holds the reply channel).
    request: Option<PickRequest>,
    /// Requests that arrived while another was open.
    queue: VecDeque<PickRequest>,
    pub title: String,
    dir: PathBuf,
    entries: Vec<Entry>,
    /// Why the folder could not be listed, or other feedback.
    note: Option<String>,
    /// Indices into `entries`, in click order.
    selected: Vec<usize>,
    scroll: usize,
    scroll_acc: f64,
    /// File name typed in Save mode.
    pub name: String,
    show_hidden: bool,
    filter: Option<usize>,
    last_click: Option<(usize, Instant)>,
    /// An existing file the user was warned about; saving it again replaces it.
    confirm_replace: Option<PathBuf>,
}

impl FilePicker {
    fn save_mode(&self) -> bool {
        self.request.as_ref().is_some_and(|r| r.save)
    }

    fn directory_mode(&self) -> bool {
        self.request.as_ref().is_some_and(|r| r.directory)
    }

    fn multiple(&self) -> bool {
        self.request.as_ref().is_some_and(|r| r.multiple)
    }

    fn has_filters(&self) -> bool {
        self.request.as_ref().is_some_and(|r| !r.filters.is_empty())
    }

    fn accept_label(&self) -> String {
        let req = self.request.as_ref();
        if let Some(label) = req.and_then(|r| r.accept_label.as_deref()) {
            // GTK-style mnemonic underscores ("_Open").
            return label.replace("__", "\u{0}").replace('_', "").replace('\u{0}', "_");
        }
        if self.save_mode() {
            "Save"
        } else if self.directory_mode() {
            "Select"
        } else {
            "Open"
        }
        .into()
    }

    fn filter_label(&self) -> String {
        let filter = self.filter.and_then(|i| self.request.as_ref()?.filters.get(i));
        match filter {
            Some(f) => format!("Type: {}", f.name),
            None => "Type: All files".into(),
        }
    }

    /// Start answering `req`.
    fn begin(&mut self, req: PickRequest) {
        let home = home_dir();
        let dir = req.folder.clone().filter(|d| d.is_dir()).unwrap_or(home);
        self.title = if req.title.trim().is_empty() {
            if req.save { "Save File" } else { "Open File" }.into()
        } else {
            req.title.clone()
        };
        self.name = req.name.clone();
        self.filter = req.current_filter.or(if req.filters.is_empty() { None } else { Some(0) });
        self.request = Some(req);
        self.enter(dir);
    }

    /// Show folder `dir`.
    fn enter(&mut self, dir: PathBuf) {
        self.dir = dir;
        self.reload();
    }

    fn reload(&mut self) {
        self.selected.clear();
        self.scroll = 0;
        self.scroll_acc = 0.0;
        self.last_click = None;
        self.confirm_replace = None;
        self.note = None;
        self.entries.clear();

        let read = match std::fs::read_dir(&self.dir) {
            Ok(r) => r,
            Err(err) => {
                self.note = Some(format!("Cannot open this folder: {err}"));
                return;
            }
        };
        let filter = self.filter.and_then(|i| self.request.as_ref()?.filters.get(i).cloned());
        let dirs_only = self.directory_mode();
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !self.show_hidden && name.starts_with('.') {
                continue;
            }
            // Follow symlinks so linked folders can be entered.
            let is_dir = std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir());
            if !is_dir && (dirs_only || filter.as_ref().is_some_and(|f| !f.matches(&name))) {
                continue;
            }
            self.entries.push(Entry { name, is_dir });
        }
        self.entries.sort_by(|a, b| {
            b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
    }

    fn up(&mut self) {
        if let Some(parent) = self.dir.parent() {
            let child = self.dir.file_name().map(|n| n.to_string_lossy().into_owned());
            self.enter(parent.to_path_buf());
            // Keep the folder we came from selected.
            if let Some(i) = child.and_then(|c| self.entries.iter().position(|e| e.name == c)) {
                self.select_only(i);
            }
        }
    }

    fn select_only(&mut self, i: usize) {
        self.selected = vec![i];
        self.confirm_replace = None;
        self.reveal(i);
    }

    fn path_of(&self, i: usize) -> PathBuf {
        self.dir.join(&self.entries[i].name)
    }

    // --- geometry -------------------------------------------------------------

    fn content_size(&self) -> (i32, i32) {
        let name = if self.save_mode() { FIELD_H + GAP } else { 0 };
        (PICK_W, PAD + BTN_H + GAP + LIST_H + GAP + name + NOTE_H + BTN_H + PAD)
    }

    fn layout(&self, c: Rect) -> PickLayout {
        let r = |x, y, w, h| Rectangle::new((x, y).into(), (w, h).into());
        let x = c.loc.x + PAD;
        let w = c.size.w - 2 * PAD;
        let mut y = c.loc.y + PAD;
        let up = r(x, y, NAV_W, BTN_H);
        let home = r(x + NAV_W + GAP, y, NAV_W, BTN_H);
        let path_x = home.loc.x + NAV_W + GAP + 4;
        let path = r(path_x, y, x + w - path_x, BTN_H);
        y += BTN_H + GAP;
        let list = r(x, y, w, LIST_H);
        y += LIST_H + GAP;
        let name_label = r(x, y, NAME_LABEL_W, FIELD_H);
        let name = self.save_mode().then(|| r(x + NAME_LABEL_W, y, w - NAME_LABEL_W, FIELD_H));
        if name.is_some() {
            y += FIELD_H + GAP;
        }
        let note = r(x, y, w, NOTE_H);
        y += NOTE_H;
        let hidden = r(x, y, HIDDEN_W, BTN_H);
        let filter = self.has_filters().then(|| r(x + HIDDEN_W + GAP, y, FILTER_W, BTN_H));
        let accept = r(x + w - ACTION_W, y, ACTION_W, BTN_H);
        let cancel = r(accept.loc.x - GAP - ACTION_W, y, ACTION_W, BTN_H);
        PickLayout { up, home, path, list, name_label, name, note, hidden, filter, cancel, accept }
    }

    fn visible_rows(&self) -> usize {
        ((LIST_H - 2 * LIST_INSET) / ROW_H).max(1) as usize
    }

    fn max_scroll(&self) -> usize {
        self.entries.len().saturating_sub(self.visible_rows())
    }

    /// Scroll so entry `i` is in view.
    fn reveal(&mut self, i: usize) {
        let visible = self.visible_rows();
        if i < self.scroll {
            self.scroll = i;
        } else if i >= self.scroll + visible {
            self.scroll = i + 1 - visible;
        }
    }

    pub fn scroll_by(&mut self, amount: f64) {
        self.scroll_acc += amount;
        let rows = (self.scroll_acc / ROW_H as f64).trunc();
        if rows != 0.0 {
            self.scroll_acc -= rows * ROW_H as f64;
            let next = (self.scroll as i64 + rows as i64).clamp(0, self.max_scroll() as i64);
            self.scroll = next as usize;
        }
    }

    fn row_rect(&self, list: Rect, slot: usize, width: i32) -> Rect {
        Rectangle::new(
            (list.loc.x + LIST_INSET, list.loc.y + LIST_INSET + slot as i32 * ROW_H).into(),
            (width, ROW_H).into(),
        )
    }

    fn hit(&self, c: Rect, text: &TextRenderer, cursor: Option<usize>, p: Point<f64, Logical>) -> Option<PanelHit> {
        let l = self.layout(c);
        let inside = |r: Rect| r.to_f64().contains(p);
        let pick = |h| Some(PanelHit::Pick(h));
        if inside(l.up) {
            return pick(PickHit::Up);
        }
        if inside(l.home) {
            return pick(PickHit::Home);
        }
        if inside(l.list) {
            let slot = ((p.y - (l.list.loc.y + LIST_INSET) as f64) / ROW_H as f64).floor();
            if slot >= 0.0 && (slot as usize) < self.visible_rows() {
                let i = self.scroll + slot as usize;
                if i < self.entries.len() {
                    return pick(PickHit::Row(i));
                }
            }
            return Some(PanelHit::Body);
        }
        if let Some(name) = l.name.filter(|r| inside(*r)) {
            let f = Field::PickName;
            let view = field_view(text, &self.name, cursor, name.size.w - 2 * FIELD_TEXT_PAD);
            let mut pos = (name.loc.x + FIELD_TEXT_PAD) as f64;
            let mut idx = view.start;
            for ch in self.name.chars().skip(view.start) {
                let adv = text.measure(&ch.to_string()) as f64;
                if p.x < pos + adv / 2.0 {
                    break;
                }
                pos += adv;
                idx += 1;
            }
            return Some(PanelHit::Field(f, idx));
        }
        if inside(l.hidden) {
            return pick(PickHit::Hidden);
        }
        if l.filter.is_some_and(inside) {
            return pick(PickHit::Filter);
        }
        if inside(l.cancel) {
            return pick(PickHit::Cancel);
        }
        if inside(l.accept) {
            return pick(PickHit::Accept);
        }
        None
    }

    // --- rendering ------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn render(
        &self,
        c: Rect,
        solids: &mut Vec<SolidColorBuffer>,
        editing: bool,
        cursor: usize,
        text: &mut TextRenderer,
        renderer: &mut GlesRenderer,
        hovered: Option<PanelHit>,
        elems: &mut Vec<CustomElement>,
    ) {
        let l = self.layout(c);
        let visible = self.visible_rows();
        // Buffers: one highlight per visible row, then the fixed ones below.
        let s = bufs(solids, visible + 12);
        let (track_i, thumb_i, list_i) = (visible, visible + 1, visible + 2);
        let (name_i, caret_i, ring_i) = (visible + 3, visible + 4, visible + 5);
        let btn_i = visible + 6; // up, home, hidden, filter, cancel, accept
        let hot = |h: PickHit| hovered == Some(PanelHit::Pick(h));

        // Toolbar.
        elems.extend(ui::text_centered(text, renderer, "Up", l.up));
        elems.extend(ui::text_centered(text, renderer, "Home", l.home));
        let path = fit_tail(text, &self.dir.display().to_string(), l.path.size.w);
        elems.extend(ui::text_in(text, renderer, &path, l.path.loc.x, l.path));

        // Listing.
        let total = self.entries.len();
        let scrollable = total > visible;
        let row_w = l.list.size.w - 2 * LIST_INSET - if scrollable { SCROLLBAR_W + 4 } else { 0 };
        let mut backs = Vec::new();
        for (slot, entry) in self.entries.iter().enumerate().skip(self.scroll).take(visible) {
            let row = self.row_rect(l.list, slot - self.scroll, row_w);
            let label = if entry.is_dir { format!("{}/", entry.name) } else { entry.name.clone() };
            let label = text.fit(&label, row.size.w - 2 * FIELD_TEXT_PAD);
            let alpha = if entry.is_dir || !self.directory_mode() { 1.0 } else { 0.5 };
            elems.extend(ui::text_in_alpha(text, renderer, &label, row.loc.x + FIELD_TEXT_PAD, row, alpha));
            if self.selected.contains(&slot) {
                backs.push((slot - self.scroll, row, ROW_SELECTED));
            } else if hovered == Some(PanelHit::Pick(PickHit::Row(slot))) {
                backs.push((slot - self.scroll, row, ROW_HOT));
            }
        }
        if total == 0 && self.note.is_none() {
            let row = self.row_rect(l.list, 0, row_w);
            let empty = if self.directory_mode() { "No folders here" } else { "Nothing here" };
            elems.extend(ui::text_in_alpha(text, renderer, empty, row.loc.x + FIELD_TEXT_PAD, row, 0.5));
        }
        for (slot, row, color) in backs {
            elems.push(ui::solid(&mut s[slot], row, color));
        }
        if scrollable {
            let track = Rectangle::new(
                (l.list.loc.x + l.list.size.w - LIST_INSET - SCROLLBAR_W, l.list.loc.y + LIST_INSET).into(),
                (SCROLLBAR_W, l.list.size.h - 2 * LIST_INSET).into(),
            );
            let thumb_h = ((visible as f64 / total as f64) * track.size.h as f64).max(20.0) as i32;
            let frac = self.scroll as f64 / self.max_scroll().max(1) as f64;
            let thumb_y = track.loc.y + (frac * (track.size.h - thumb_h) as f64) as i32;
            let thumb = Rectangle::new((track.loc.x, thumb_y).into(), (SCROLLBAR_W, thumb_h).into());
            elems.push(ui::solid(&mut s[thumb_i], thumb, SCROLL_THUMB));
            elems.push(ui::solid(&mut s[track_i], track, SCROLL_TRACK));
        }
        elems.push(ui::solid(&mut s[list_i], l.list, FIELD_BG));

        // File name (Save).
        if let Some(name) = l.name {
            elems.extend(ui::text_in(text, renderer, "Name", l.name_label.loc.x, l.name_label));
            let tx = name.loc.x + FIELD_TEXT_PAD;
            let view = field_view(text, &self.name, editing.then_some(cursor), name.size.w - 2 * FIELD_TEXT_PAD);
            elems.extend(ui::text_in(text, renderer, &view.visible, tx, name));
            if let Some(cx) = view.caret_x {
                let caret = Rectangle::new((tx + cx, name.loc.y + 4).into(), (1, name.size.h - 8).into());
                elems.push(ui::solid(&mut s[caret_i], caret, CARET));
            }
            elems.push(ui::solid(&mut s[name_i], name, if editing { FIELD_BG_EDIT } else { FIELD_BG }));
            if editing {
                elems.push(ui::solid(&mut s[ring_i], edit_ring(name), FIELD_EDIT_RING));
            }
        }

        if let Some(note) = &self.note {
            let note = text.fit(note, l.note.size.w);
            elems.extend(ui::text_in_alpha(text, renderer, &note, l.note.loc.x, l.note, 0.8));
        }

        // Buttons.
        let hidden_label = if self.show_hidden { "Hidden files: on" } else { "Hidden files: off" };
        let mut buttons = vec![
            (l.up, "Up".to_string(), hot(PickHit::Up), false),
            (l.home, "Home".to_string(), hot(PickHit::Home), false),
            (l.hidden, hidden_label.to_string(), hot(PickHit::Hidden), false),
        ];
        if let Some(filter) = l.filter {
            let label = text.fit(&self.filter_label(), filter.size.w - 12);
            buttons.push((filter, label, hot(PickHit::Filter), false));
        }
        buttons.push((l.cancel, "Cancel".into(), hot(PickHit::Cancel), false));
        buttons.push((l.accept, self.accept_label(), hot(PickHit::Accept), true));
        for (k, (rect, label, is_hot, primary)) in buttons.into_iter().enumerate() {
            // Up and Home labels were drawn with the toolbar.
            if k >= 2 {
                elems.extend(ui::text_centered(text, renderer, &label, rect));
            }
            let color = match (primary, is_hot) {
                (true, true) => ACCEPT_HOT,
                (true, false) => ACCEPT,
                (false, true) => BUTTON_HOT,
                (false, false) => BUTTON,
            };
            elems.push(ui::solid(&mut s[btn_i + k], rect, color));
        }
    }
}

/// `s` shortened from the left with a leading ellipsis to fit `width`.
fn fit_tail(text: &TextRenderer, s: &str, width: i32) -> String {
    if text.measure(s) <= width {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    for start in 1..chars.len() {
        let candidate: String = std::iter::once('…').chain(chars[start..].iter().copied()).collect();
        if text.measure(&candidate) <= width {
            return candidate;
        }
    }
    String::new()
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// Expand a leading `~` in a typed path.
fn expand_tilde(s: &str) -> PathBuf {
    if s == "~" {
        return home_dir();
    }
    match s.strip_prefix("~/") {
        Some(rest) => home_dir().join(rest),
        None => PathBuf::from(s),
    }
}

// --- Panels glue ---------------------------------------------------------------

impl super::Panels {
    pub(super) fn picker_size(&self) -> (i32, i32) {
        self.picker.content_size()
    }

    pub(super) fn picker_hit(&self, text: &TextRenderer, p: Point<f64, Logical>) -> Option<PanelHit> {
        let cursor = (self.editing == Some(Field::PickName)).then_some(self.cursor);
        self.picker.hit(self.content_rect(PanelId::FilePicker), text, cursor, p)
    }

    pub(super) fn render_picker(
        &mut self,
        text: &mut TextRenderer,
        renderer: &mut GlesRenderer,
        hovered: Option<PanelHit>,
        elems: &mut Vec<CustomElement>,
    ) {
        let c = self.content_rect(PanelId::FilePicker);
        let editing = self.editing == Some(Field::PickName);
        self.picker
            .render(c, &mut self.file_picker.solids, editing, self.cursor, text, renderer, hovered, elems);
    }
}

// --- Compositor side of portal requests ----------------------------------------

impl State {
    pub fn portal_event(&mut self, event: PortalEvent) {
        match event {
            PortalEvent::Pick(req) => self.request_pick(req),
            PortalEvent::Close(id) => {
                if self.panels.picker.request.as_ref().is_some_and(|r| r.id == id) {
                    self.finish_pick(PickResult::Failed);
                } else if let Some(i) = self.panels.picker.queue.iter().position(|r| r.id == id) {
                    if let Some(PickRequest { reply: PickReply::Portal(tx), .. }) =
                        self.panels.picker.queue.remove(i)
                    {
                        let _ = tx.send(PickResult::Failed);
                    }
                }
            }
        }
    }

    /// Show the File Picker for `req`, or queue it behind the one already open.
    pub fn request_pick(&mut self, req: PickRequest) {
        if self.panels.picker.request.is_some() {
            self.panels.picker.queue.push_back(req);
        } else {
            self.open_picker(req);
        }
    }

    /// Whether a request from one of simplewm's own applets matching `is` is open
    /// or queued. If it is the open one, the File Picker is raised.
    fn applet_pick_pending(&mut self, is: impl Fn(&PickReply) -> bool) -> bool {
        let p = &self.panels.picker;
        if p.request.as_ref().is_some_and(|r| is(&r.reply)) {
            self.focus_panel(PanelId::FilePicker);
            return true;
        }
        p.queue.iter().any(|r| is(&r.reply))
    }

    /// A File Picker request from one of simplewm's own applets (id 0: the portal
    /// never closes it).
    fn applet_pick(&mut self, title: &str, directory: bool, multiple: bool, folder: Option<PathBuf>, reply: PickReply) {
        self.request_pick(PickRequest {
            id: 0,
            save: false,
            multiple,
            directory,
            title: title.into(),
            accept_label: None,
            folder,
            name: String::new(),
            filters: Vec::new(),
            current_filter: None,
            reply,
        });
    }

    /// The Screenshot applet's Choose button: pick the screenshot folder.
    pub fn choose_screenshot_dir(&mut self) {
        if self.applet_pick_pending(|r| matches!(r, PickReply::ScreenshotDir)) {
            return;
        }
        let folder = self.panels.screenshot_dir();
        self.applet_pick("Choose Screenshot Folder", true, false, folder, PickReply::ScreenshotDir);
    }

    /// The Launcher applet's Add (`None`) or Change (`Some(i)`) button: pick
    /// executables. Only one of these is open or queued at a time.
    pub fn choose_launcher_entry(&mut self, change: Option<usize>) {
        if self.applet_pick_pending(|r| matches!(r, PickReply::LauncherAdd | PickReply::LauncherChange(_))) {
            return;
        }
        match change.and_then(|i| self.panels.entries.get(i).cloned()) {
            Some(old) => {
                let folder = Path::new(&old).parent().map(Path::to_path_buf);
                self.applet_pick("Change Launcher Entry", false, false, folder, PickReply::LauncherChange(old));
            }
            None => {
                let folder = Some(PathBuf::from("/usr/bin"));
                self.applet_pick("Add to Launcher", false, true, folder, PickReply::LauncherAdd);
            }
        }
    }

    fn open_picker(&mut self, req: PickRequest) {
        let save = req.save;
        self.panels.picker.begin(req);
        self.panels.forget_loc(PanelId::FilePicker);
        let out = self.output_geometry();
        self.panels.show(PanelId::FilePicker, out);
        self.focus_panel(PanelId::FilePicker);
        if save {
            let end = self.panels.picker.name.chars().count();
            // Put the caret before the extension, like other file dialogs.
            let stem = Path::new(&self.panels.picker.name)
                .file_stem()
                .map(|s| s.to_string_lossy().chars().count())
                .unwrap_or(end);
            self.panels.begin_edit(Field::PickName, stem);
        }
    }

    /// Answer the open request and close the File Picker.
    pub fn finish_pick(&mut self, result: PickResult) {
        if let Some(req) = self.panels.picker.request.take() {
            match req.reply {
                PickReply::Portal(tx) => {
                    let _ = tx.send(result);
                }
                reply => {
                    if let PickResult::Chosen(paths) = result
                        && let Some(first) = paths.first()
                    {
                        match reply {
                            PickReply::ScreenshotDir => self.panels.set_screenshot_dir(first),
                            PickReply::LauncherAdd => self.panels.add_entries(&paths),
                            PickReply::LauncherChange(old) => self.panels.change_entry(&old, first),
                            PickReply::Portal(_) => {}
                        }
                    }
                }
            }
        }
        self.panels.close(PanelId::FilePicker);
        self.focus_top();
        if let Some(next) = self.panels.picker.queue.pop_front() {
            self.open_picker(next);
        }
    }

    pub fn picker_click(&mut self, hit: PickHit) {
        let ctrl = self.seat.get_keyboard().is_some_and(|k| k.modifier_state().ctrl);
        let p = &mut self.panels.picker;
        match hit {
            PickHit::Up => p.up(),
            PickHit::Home => p.enter(home_dir()),
            PickHit::Hidden => {
                p.show_hidden = !p.show_hidden;
                p.reload();
            }
            PickHit::Filter => {
                let n = p.request.as_ref().map_or(0, |r| r.filters.len());
                p.filter = match p.filter {
                    None if n > 0 => Some(0),
                    Some(i) if i + 1 < n => Some(i + 1),
                    _ => None,
                };
                p.reload();
            }
            PickHit::Cancel => self.finish_pick(PickResult::Cancelled),
            PickHit::Accept => self.picker_accept(),
            PickHit::Row(i) => {
                let now = Instant::now();
                let double = p.last_click.is_some_and(|(j, t)| j == i && now - t < DOUBLE_CLICK);
                p.last_click = if double { None } else { Some((i, now)) };
                if double {
                    self.picker_activate(i);
                    return;
                }
                if ctrl && p.multiple() {
                    match p.selected.iter().position(|&s| s == i) {
                        Some(k) => {
                            p.selected.remove(k);
                        }
                        None => p.selected.push(i),
                    }
                } else {
                    p.select_only(i);
                }
                if p.save_mode() && !p.entries[i].is_dir {
                    p.name = p.entries[i].name.clone();
                    let end = p.name.chars().count();
                    self.panels.begin_edit(Field::PickName, end);
                }
            }
        }
    }

    /// Double-click or Enter on entry `i`: enter a folder, or choose a file.
    fn picker_activate(&mut self, i: usize) {
        let p = &mut self.panels.picker;
        let Some(entry) = p.entries.get(i).cloned() else {
            return;
        };
        if entry.is_dir {
            let path = p.path_of(i);
            p.enter(path);
        } else if p.save_mode() {
            p.name = entry.name;
            self.picker_accept();
        } else if !p.directory_mode() {
            let path = p.path_of(i);
            self.finish_pick(PickResult::Chosen(vec![path]));
        }
    }

    /// The accept button (Open / Save / Select) or Enter.
    fn picker_accept(&mut self) {
        let p = &mut self.panels.picker;
        if p.save_mode() {
            let name = p.name.trim().to_string();
            if name.is_empty() {
                if let [i] = p.selected[..] {
                    if p.entries[i].is_dir {
                        let path = p.path_of(i);
                        p.enter(path);
                    }
                }
                return;
            }
            let path = p.dir.join(expand_tilde(&name));
            if path.is_dir() {
                p.name.clear();
                p.enter(path);
                self.panels.cursor = 0;
                return;
            }
            if !path.parent().is_some_and(Path::is_dir) {
                p.note = Some("That folder does not exist.".into());
                return;
            }
            if path.exists() && p.confirm_replace.as_ref() != Some(&path) {
                let label = p.accept_label();
                p.note = Some(format!("A file with this name already exists. {label} again to replace it."));
                p.confirm_replace = Some(path);
                return;
            }
            self.finish_pick(PickResult::Chosen(vec![path]));
            return;
        }

        let chosen: Vec<usize> = p.selected.clone();
        if p.directory_mode() {
            let paths = if chosen.is_empty() {
                vec![p.dir.clone()]
            } else {
                chosen.iter().map(|&i| p.path_of(i)).collect()
            };
            self.finish_pick(PickResult::Chosen(paths));
            return;
        }
        match chosen[..] {
            [] => {}
            [i] if p.entries[i].is_dir => {
                let path = p.path_of(i);
                p.enter(path);
            }
            _ => {
                let files: Vec<PathBuf> =
                    chosen.iter().filter(|&&i| !p.entries[i].is_dir).map(|&i| p.path_of(i)).collect();
                if !files.is_empty() {
                    self.finish_pick(PickResult::Chosen(files));
                }
            }
        }
    }

    /// A key pressed while the File Picker has focus.
    pub fn picker_key(&mut self, sym: u32, ch: Option<char>, ctrl: bool) {
        let editing = self.panels.editing == Some(Field::PickName);
        match sym {
            keysyms::KEY_Escape => self.finish_pick(PickResult::Cancelled),
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => {
                let p = &self.panels.picker;
                match p.selected[..] {
                    // A single highlighted entry: same as double-clicking it, unless a
                    // typed save name should win.
                    [i] if !editing || p.entries[i].is_dir => self.picker_activate(i),
                    _ => self.picker_accept(),
                }
            }
            keysyms::KEY_Up | keysyms::KEY_KP_Up | keysyms::KEY_Down | keysyms::KEY_KP_Down => {
                let p = &mut self.panels.picker;
                if p.entries.is_empty() {
                    return;
                }
                let down = matches!(sym, keysyms::KEY_Down | keysyms::KEY_KP_Down);
                let next = match (p.selected.last(), down) {
                    (None, _) => 0,
                    (Some(&i), true) => (i + 1).min(p.entries.len() - 1),
                    (Some(&i), false) => i.saturating_sub(1),
                };
                p.select_only(next);
            }
            _ if editing => self.panels.key(sym, ch, ctrl),
            keysyms::KEY_BackSpace => self.panels.picker.up(),
            _ => {}
        }
    }
}
