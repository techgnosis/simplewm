//! Compositor-drawn UI: top bar, menus, window decorations and the cursor.
//! Contains layout, hit testing and render element generation.

use std::{
    cell::RefCell,
    time::{Duration, Instant},
};

use smithay::{
    backend::renderer::{
        element::{
            AsRenderElements, Kind,
            memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            render_elements,
            solid::{SolidColorBuffer, SolidColorRenderElement},
            surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
        },
        gles::GlesRenderer,
    },
    desktop::Window,
    input::pointer::{CursorImageStatus, CursorImageSurfaceData},
    reexports::wayland_server::{Resource, protocol::wl_surface::WlSurface},
    utils::{Logical, Point, Rectangle, Scale},
    wayland::{compositor::with_states, shell::xdg::XdgToplevelSurfaceData},
};

use crate::{
    grabs::Edges,
    panel::{PanelHit, PanelId},
    screenshot::Selecting,
    state::{StackEntry, State},
    text::TextRenderer,
};

// ---------------------------------------------------------------------------
// Layout constants
// ---------------------------------------------------------------------------

pub const BAR_HEIGHT: i32 = 30;
pub const TITLE_HEIGHT: i32 = 28;
pub const BORDER: i32 = 4;
/// Invisible extra grab area outside the visible border for resizing.
pub const RESIZE_MARGIN: i32 = 6;
pub const CORNER: i32 = 18;
pub const BUTTON_H: i32 = 20;
pub const CLOSE_W: i32 = 22;
pub const BUTTON_GAP: i32 = 6;
pub const BAR_BUTTON_PAD: i32 = 12;
pub const MENU_ITEM_H: i32 = 26;
pub const MENU_PAD: i32 = 4;
pub const MENU_MIN_W: i32 = 140;
// Size menu icons: a small "screen" with the target region filled in.
pub const ICON_W: i32 = 48;
pub const ICON_H: i32 = 30;
pub const ICON_PAD: i32 = 5;
pub const ICON_CELL_W: i32 = ICON_W + 2 * ICON_PAD;
pub const ICON_CELL_H: i32 = ICON_H + 2 * ICON_PAD;

// Colors (premultiplied RGBA)
const BG_COLOR: [f32; 4] = [0.16, 0.18, 0.22, 1.0];
const BAR_COLOR: [f32; 4] = [0.10, 0.11, 0.13, 1.0];
const BAR_BUTTON: [f32; 4] = [0.20, 0.22, 0.26, 1.0];
const BAR_BUTTON_HOT: [f32; 4] = [0.30, 0.33, 0.40, 1.0];
const MENU_BG: [f32; 4] = [0.14, 0.15, 0.18, 1.0];
const MENU_HL: [f32; 4] = [0.25, 0.42, 0.70, 1.0];
pub const TITLE_FOCUSED: [f32; 4] = [0.22, 0.32, 0.50, 1.0];
pub const TITLE_UNFOCUSED: [f32; 4] = [0.27, 0.28, 0.31, 1.0];
pub const CLOSE_COLOR: [f32; 4] = [0.80, 0.18, 0.18, 1.0];
pub const CLOSE_HOT: [f32; 4] = [0.95, 0.30, 0.30, 1.0];
const SIZE_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 0.25];
const ICON_SCREEN: [f32; 4] = [0.30, 0.32, 0.37, 1.0];
const ICON_FILL: [f32; 4] = [0.45, 0.62, 0.90, 1.0];
const ICON_FILL_HOT: [f32; 4] = [0.95, 0.97, 1.0, 1.0];
const SIZE_HOT: [f32; 4] = [1.0 * 0.2, 1.0 * 0.2, 1.0 * 0.2, 0.2];

// Screenshot selection
const SHOT_DIM: [f32; 4] = [0.0, 0.0, 0.0, 0.45];
const SHOT_EDGE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const CROSS_DARK: [f32; 4] = [0.0, 0.0, 0.0, 0.8];
const CROSS_ARM: i32 = 10;
/// Solid buffers used by the selection overlay: crosshair (2 light + 2
/// dark), 4 dim rects, 4 edges.
const SHOT_BUFS: usize = 12;

// Toast
const TOAST_BG: [f32; 4] = [0.10, 0.11, 0.13, 0.92];
const TOAST_EDGE: [f32; 4] = [0.25, 0.42, 0.70, 1.0];
const TOAST_PAD: i32 = 14;
const TOAST_H: i32 = 32;
const TOAST_TIME: Duration = Duration::from_millis(2500);

/// Outline drawn on top for windows hidden entirely behind others.
const HIDDEN_OUTLINE: [f32; 4] = [0.90, 0.15, 0.15, 1.0];

pub const CLEAR_COLOR: [f32; 4] = BG_COLOR;

render_elements! {
    pub CustomElement<=GlesRenderer>;
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Solid=SolidColorRenderElement,
    Memory=MemoryRenderBufferRenderElement<GlesRenderer>,
}

// ---------------------------------------------------------------------------
// Window frame geometry
// ---------------------------------------------------------------------------

pub struct Frame {
    #[allow(dead_code)]
    pub content: Rectangle<i32, Logical>,
    /// Visible frame (title bar + borders + content)
    pub frame: Rectangle<i32, Logical>,
    pub title: Rectangle<i32, Logical>,
    pub left: Rectangle<i32, Logical>,
    pub right: Rectangle<i32, Logical>,
    pub bottom: Rectangle<i32, Logical>,
    pub close: Rectangle<i32, Logical>,
    pub size: Rectangle<i32, Logical>,
    pub title_text_x: i32,
    pub title_text_max: i32,
}

/// Visible frame (title bar + borders + content) around a content rect.
pub fn frame_rect(content: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    Rectangle::new(
        content.loc - Point::from((BORDER, TITLE_HEIGHT)),
        content.size + (2 * BORDER, TITLE_HEIGHT + BORDER).into(),
    )
}

/// The four BORDER-thick edges just inside `r`.
fn outline(r: Rectangle<i32, Logical>) -> [Rectangle<i32, Logical>; 4] {
    let (x, y, w, h) = (r.loc.x, r.loc.y, r.size.w, r.size.h);
    [
        Rectangle::new((x, y).into(), (w, BORDER).into()),
        Rectangle::new((x, y + h - BORDER).into(), (w, BORDER).into()),
        Rectangle::new((x, y).into(), (BORDER, h).into()),
        Rectangle::new((x + w - BORDER, y).into(), (BORDER, h).into()),
    ]
}

pub fn size_button_width(text: &TextRenderer) -> i32 {
    text.measure("Size") + 16
}

impl Frame {
    pub fn new(content: Rectangle<i32, Logical>, text: &TextRenderer) -> Self {
        let (x, y, w, h) = (content.loc.x, content.loc.y, content.size.w, content.size.h);
        let frame = frame_rect(content);
        let title = Rectangle::new(frame.loc, (frame.size.w, TITLE_HEIGHT).into());
        let left = Rectangle::new((x - BORDER, y).into(), (BORDER, h).into());
        let right = Rectangle::new((x + w, y).into(), (BORDER, h).into());
        let bottom = Rectangle::new((x - BORDER, y + h).into(), (w + 2 * BORDER, BORDER).into());
        let by = title.loc.y + (TITLE_HEIGHT - BUTTON_H) / 2;
        let close_x = title.loc.x + title.size.w - BORDER - 2 - CLOSE_W;
        let close = Rectangle::new((close_x, by).into(), (CLOSE_W, BUTTON_H).into());
        let sw = size_button_width(text);
        let size = Rectangle::new(
            (close_x - BUTTON_GAP - sw, by).into(),
            (sw, BUTTON_H).into(),
        );
        let title_text_x = title.loc.x + BORDER + 8;
        let title_text_max = size.loc.x - BUTTON_GAP - title_text_x;
        Self {
            content,
            frame,
            title,
            left,
            right,
            bottom,
            close,
            size,
            title_text_x,
            title_text_max,
        }
    }

    pub fn outer(&self) -> Rectangle<i32, Logical> {
        let mut r = self.frame;
        r.loc -= (RESIZE_MARGIN, RESIZE_MARGIN).into();
        r.size += (2 * RESIZE_MARGIN, 2 * RESIZE_MARGIN).into();
        r
    }

    pub fn edges_at(&self, p: Point<f64, Logical>) -> Edges {
        let f = self.frame;
        let (fx, fy) = (f.loc.x as f64, f.loc.y as f64);
        let (fr, fb) = ((f.loc.x + f.size.w) as f64, (f.loc.y + f.size.h) as f64);
        let b = BORDER as f64;
        let c = CORNER as f64;
        let mut e = Edges {
            left: p.x < fx + b,
            right: p.x >= fr - b,
            top: p.y < fy + 3.0,
            bottom: p.y >= fb - b,
        };
        if e.top || e.bottom {
            e.left |= p.x < fx + c;
            e.right |= p.x >= fr - c;
        }
        if e.left || e.right {
            e.top |= p.y < fy + c;
            e.bottom |= p.y >= fb - c;
        }
        e
    }
}

// ---------------------------------------------------------------------------
// Bar + menus
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarButton {
    ControlPanel,
    Launcher,
    Screenshot,
}

impl BarButton {
    pub fn label(self) -> &'static str {
        match self {
            BarButton::ControlPanel => "Control Panel",
            BarButton::Launcher => "Launcher",
            BarButton::Screenshot => "Screenshot",
        }
    }
}

pub const CLOCK_FORMAT: &str = "%a %e %b  %H:%M";
const BAR_GAP: i32 = 6;
const CLOCK_GAP: i32 = 16;

pub fn clock_text() -> String {
    chrono::Local::now().format(CLOCK_FORMAT).to_string()
}

/// Width reserved for the clock, so the centered group doesn't shift as the
/// day/month names change width.
fn clock_slot_width(text: &TextRenderer) -> i32 {
    ["Wed 30 Sep  00:00", "Mon 31 May  00:00"]
        .iter()
        .map(|s| text.measure(s))
        .max()
        .unwrap()
}

const BAR_BUTTONS: [BarButton; 3] = [
    BarButton::ControlPanel,
    BarButton::Launcher,
    BarButton::Screenshot,
];
type BarRects = [(BarButton, Rectangle<i32, Logical>); BAR_BUTTONS.len()];

/// The bar items (Control Panel, Launcher, Screenshot, clock) are laid out as
/// one group centered in the bar. Returns the button rects (in global
/// coordinates) and the clock's x position.
pub fn bar_layout(text: &TextRenderer, out: Rectangle<i32, Logical>) -> (BarRects, i32) {
    let labels = BAR_BUTTONS;
    let widths = labels.map(|b| text.measure(b.label()) + 2 * BAR_BUTTON_PAD);
    let clock_w = clock_slot_width(text);
    let total = widths.iter().sum::<i32>() + BAR_GAP + CLOCK_GAP + clock_w;

    let mut x = out.loc.x + (out.size.w - total) / 2;
    let y = out.loc.y + 4;
    let h = BAR_HEIGHT - 8;
    let mut rects = [(BarButton::ControlPanel, Rectangle::default()); BAR_BUTTONS.len()];
    for (i, b) in labels.into_iter().enumerate() {
        rects[i] = (b, Rectangle::new((x, y).into(), (widths[i], h).into()));
        x += widths[i] + BAR_GAP;
    }
    let clock_x = x - BAR_GAP + CLOCK_GAP;
    (rects, clock_x)
}

pub fn bar_buttons(text: &TextRenderer, out: Rectangle<i32, Logical>) -> BarRects {
    bar_layout(text, out).0
}

/// Size menu options: label and the target region in halves of the usable
/// area, as (x0, y0, x1, y1) where 0 = start, 1 = middle, 2 = end.
pub const SIZE_OPTIONS: [(&str, [i32; 4]); 7] = [
    ("Left half", [0, 0, 1, 2]),
    ("Right half", [1, 0, 2, 2]),
    ("Top left", [0, 0, 1, 1]),
    ("Top right", [1, 0, 2, 1]),
    ("Bottom left", [0, 1, 1, 2]),
    ("Bottom right", [1, 1, 2, 2]),
    ("Full screen", [0, 0, 2, 2]),
];

const SIZE_LABELS: [&str; 7] = {
    let mut labels = [""; 7];
    let mut i = 0;
    while i < 7 {
        labels[i] = SIZE_OPTIONS[i].0;
        i += 1;
    }
    labels
};

#[derive(Clone, PartialEq)]
pub enum MenuKind {
    /// Executable paths from the Control Panel's Launcher section.
    Launcher(Vec<String>),
    Size(Window),
}

impl MenuKind {
    pub fn items(&self) -> Vec<String> {
        match self {
            MenuKind::Launcher(paths) if paths.is_empty() => {
                vec!["Add apps in Control Panel".into()]
            }
            MenuKind::Launcher(paths) => paths.iter().map(|p| crate::panel::exe_name(p)).collect(),
            MenuKind::Size(_) => SIZE_LABELS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

pub struct OpenMenu {
    pub kind: MenuKind,
    pub rect: Rectangle<i32, Logical>,
}

impl OpenMenu {
    pub fn item_rect(&self, i: usize) -> Rectangle<i32, Logical> {
        if let MenuKind::Size(_) = self.kind {
            return Rectangle::new(
                (
                    self.rect.loc.x + MENU_PAD + i as i32 * ICON_CELL_W,
                    self.rect.loc.y + MENU_PAD,
                )
                    .into(),
                (ICON_CELL_W, ICON_CELL_H).into(),
            );
        }
        Rectangle::new(
            (
                self.rect.loc.x + MENU_PAD,
                self.rect.loc.y + MENU_PAD + i as i32 * MENU_ITEM_H,
            )
                .into(),
            (self.rect.size.w - 2 * MENU_PAD, MENU_ITEM_H).into(),
        )
    }

    pub fn item_at(&self, p: Point<f64, Logical>) -> Option<usize> {
        (0..self.kind.items().len()).find(|&i| self.item_rect(i).to_f64().contains(p))
    }

    /// Row under the icons of the Size menu that names the hovered option.
    pub fn label_rect(&self) -> Rectangle<i32, Logical> {
        Rectangle::new(
            (
                self.rect.loc.x + MENU_PAD,
                self.rect.loc.y + MENU_PAD + ICON_CELL_H,
            )
                .into(),
            (self.rect.size.w - 2 * MENU_PAD, MENU_ITEM_H).into(),
        )
    }
}

/// The small screen drawn for a Size option, and the filled target region.
fn size_icon(
    cell: Rectangle<i32, Logical>,
    region: [i32; 4],
) -> (Rectangle<i32, Logical>, Rectangle<i32, Logical>) {
    let screen = Rectangle::new(
        (cell.loc.x + ICON_PAD, cell.loc.y + ICON_PAD).into(),
        (ICON_W, ICON_H).into(),
    );
    // Inner area, inset so the screen shows as a frame; regions get a 1px gap.
    let inset = 2;
    let ix = [
        screen.loc.x + inset,
        screen.loc.x + ICON_W / 2,
        screen.loc.x + ICON_W - inset,
    ];
    let iy = [
        screen.loc.y + inset,
        screen.loc.y + ICON_H / 2,
        screen.loc.y + ICON_H - inset,
    ];
    let [x0, y0, x1, y1] = region.map(|v| v as usize);
    let gap = |v: usize| if v == 1 { 1 } else { 0 };
    let left = ix[x0] + gap(x0);
    let top = iy[y0] + gap(y0);
    let right = ix[x1] - gap(x1);
    let bottom = iy[y1] - gap(y1);
    let fill = Rectangle::new((left, top).into(), (right - left, bottom - top).into());
    (screen, fill)
}

// ---------------------------------------------------------------------------
// Hit testing
// ---------------------------------------------------------------------------

pub enum Hit {
    Menu(Option<usize>),
    Panel(PanelId, PanelHit),
    BarButton(BarButton),
    Bar,
    Close(Window),
    SizeButton(Window),
    Title(Window),
    Resize(Window, Edges),
    Content(Window, WlSurface, Point<f64, Logical>),
    /// Part of a window that only focuses it.
    Inert(Window),
    Background,
}

impl State {
    pub fn hit_test(&self, pos: Point<f64, Logical>) -> Hit {
        if let Some(menu) = &self.ui.menu
            && menu.rect.to_f64().contains(pos)
        {
            return Hit::Menu(menu.item_at(pos));
        }

        let out = self.output_geometry();
        if pos.y < (out.loc.y + BAR_HEIGHT) as f64 {
            for (b, r) in bar_buttons(&self.ui.text, out) {
                if r.to_f64().contains(pos) {
                    return Hit::BarButton(b);
                }
            }
            return Hit::Bar;
        }

        for item in self.stack_top_down() {
            let window = match item {
                StackEntry::Panel(id) => {
                    if let Some(h) = self.panels.hit(id, &self.ui.text, pos) {
                        return Hit::Panel(id, h);
                    }
                    continue;
                }
                StackEntry::Window(w) => w,
            };
            let window = &window;
            let Some(content) = self.space.element_geometry(window) else {
                continue;
            };
            if let Some((surface, loc)) = self.surface_under_in_window(window, pos) {
                return Hit::Content(window.clone(), surface, loc);
            }
            let frame = Frame::new(content, &self.ui.text);
            if !frame.outer().to_f64().contains(pos) {
                continue;
            }
            if frame.close.to_f64().contains(pos) {
                return Hit::Close(window.clone());
            }
            if frame.size.to_f64().contains(pos) {
                return Hit::SizeButton(window.clone());
            }
            let edges = frame.edges_at(pos);
            if edges.any() {
                return Hit::Resize(window.clone(), edges);
            }
            if frame.title.to_f64().contains(pos) {
                return Hit::Title(window.clone());
            }
            return Hit::Inert(window.clone());
        }

        Hit::Background
    }
}

// ---------------------------------------------------------------------------
// Ui state + rendering
// ---------------------------------------------------------------------------

fn solid_buffer() -> SolidColorBuffer {
    SolidColorBuffer::new((0, 0), [0.0; 4])
}

struct DecoBuffers {
    title: SolidColorBuffer,
    left: SolidColorBuffer,
    right: SolidColorBuffer,
    bottom: SolidColorBuffer,
    close: SolidColorBuffer,
    size: SolidColorBuffer,
}

impl Default for DecoBuffers {
    fn default() -> Self {
        Self {
            title: solid_buffer(),
            left: solid_buffer(),
            right: solid_buffer(),
            bottom: solid_buffer(),
            close: solid_buffer(),
            size: solid_buffer(),
        }
    }
}

pub struct Ui {
    pub text: TextRenderer,
    pub menu: Option<OpenMenu>,
    /// Screenshot selection mode.
    pub shot: Option<Selecting>,
    /// Short message shown below the bar, and when it was posted.
    toast: Option<(String, Instant)>,
    bar_bg: SolidColorBuffer,
    bar_buttons: [SolidColorBuffer; BAR_BUTTONS.len()],
    shot_bufs: Vec<SolidColorBuffer>,
    toast_bufs: [SolidColorBuffer; 2],
    menu_bg: SolidColorBuffer,
    menu_hl: SolidColorBuffer,
    icon_screens: Vec<SolidColorBuffer>,
    icon_fills: Vec<SolidColorBuffer>,
    /// Edge buffers for hidden-window outlines, four per hidden window.
    hidden_outlines: Vec<SolidColorBuffer>,
    cursor: MemoryRenderBuffer,
}

impl Ui {
    pub fn new() -> Self {
        Self {
            text: TextRenderer::new(),
            menu: None,
            shot: None,
            toast: None,
            bar_bg: solid_buffer(),
            bar_buttons: BAR_BUTTONS.map(|_| solid_buffer()),
            shot_bufs: (0..SHOT_BUFS).map(|_| solid_buffer()).collect(),
            toast_bufs: [solid_buffer(), solid_buffer()],
            menu_bg: solid_buffer(),
            menu_hl: solid_buffer(),
            icon_screens: SIZE_OPTIONS.iter().map(|_| solid_buffer()).collect(),
            icon_fills: SIZE_OPTIONS.iter().map(|_| solid_buffer()).collect(),
            hidden_outlines: Vec::new(),
            cursor: crate::text::arrow_cursor(),
        }
    }

    pub fn open_menu(
        &mut self,
        kind: MenuKind,
        anchor: Point<i32, Logical>,
        out: Rectangle<i32, Logical>,
    ) {
        let items = kind.items();
        let (w, h) = if let MenuKind::Size(_) = kind {
            (
                items.len() as i32 * ICON_CELL_W + 2 * MENU_PAD,
                ICON_CELL_H + MENU_ITEM_H + 2 * MENU_PAD,
            )
        } else {
            self.list_menu_size(&items)
        };
        let mut loc = anchor;
        loc.x = loc.x.min(out.loc.x + out.size.w - w).max(out.loc.x);
        loc.y = loc.y.min(out.loc.y + out.size.h - h).max(out.loc.y);
        self.menu = Some(OpenMenu {
            kind,
            rect: Rectangle::new(loc, (w, h).into()),
        });
    }

    fn list_menu_size(&self, items: &[String]) -> (i32, i32) {
        let w = items
            .iter()
            .map(|t| self.text.measure(t) + 2 * BAR_BUTTON_PAD)
            .max()
            .unwrap_or(0)
            .max(MENU_MIN_W)
            + 2 * MENU_PAD;
        let h = items.len() as i32 * MENU_ITEM_H + 2 * MENU_PAD;
        (w, h)
    }

    pub fn show_toast(&mut self, message: &str) {
        self.toast = Some((message.to_string(), Instant::now()));
    }

    pub fn forget_window(&mut self, window: &Window) {
        if matches!(&self.menu, Some(OpenMenu { kind: MenuKind::Size(w), .. }) if w == window) {
            self.menu = None;
        }
    }
}

pub(crate) fn solid(
    buf: &mut SolidColorBuffer,
    rect: Rectangle<i32, Logical>,
    color: [f32; 4],
) -> CustomElement {
    buf.update(rect.size, color);
    SolidColorRenderElement::from_buffer(
        buf,
        rect.loc.to_physical(1),
        Scale::from(1.0),
        1.0,
        Kind::Unspecified,
    )
    .into()
}

fn text_elem(
    text: &mut TextRenderer,
    renderer: &mut GlesRenderer,
    s: &str,
    loc: Point<i32, Logical>,
    alpha: f32,
) -> Option<CustomElement> {
    let (buf, _) = text.get(s)?;
    MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        loc.to_f64().to_physical(1.0),
        &buf,
        Some(alpha),
        None,
        None,
        Kind::Unspecified,
    )
    .ok()
    .map(CustomElement::from)
}

/// Text vertically centered in `rect`, starting at x.
pub(crate) fn text_in(
    text: &mut TextRenderer,
    renderer: &mut GlesRenderer,
    s: &str,
    x: i32,
    rect: Rectangle<i32, Logical>,
) -> Option<CustomElement> {
    text_in_alpha(text, renderer, s, x, rect, 1.0)
}

/// Like `text_in`, faded by `alpha`.
pub(crate) fn text_in_alpha(
    text: &mut TextRenderer,
    renderer: &mut GlesRenderer,
    s: &str,
    x: i32,
    rect: Rectangle<i32, Logical>,
    alpha: f32,
) -> Option<CustomElement> {
    let y = rect.loc.y + (rect.size.h - text.line_height()) / 2;
    text_elem(text, renderer, s, (x, y).into(), alpha)
}

/// Text centered in `rect`.
pub(crate) fn text_centered(
    text: &mut TextRenderer,
    renderer: &mut GlesRenderer,
    s: &str,
    rect: Rectangle<i32, Logical>,
) -> Option<CustomElement> {
    let x = rect.loc.x + (rect.size.w - text.measure(s)) / 2;
    text_in(text, renderer, s, x, rect)
}

pub fn window_title(window: &Window) -> String {
    let Some(toplevel) = window.toplevel() else {
        return String::new();
    };
    with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|d| d.lock().unwrap().title.clone())
            .unwrap_or_default()
    })
}

/// Whether `window` is a nested gamescope session (by its app ID).
fn is_gamescope(window: &Window) -> bool {
    let Some(toplevel) = window.toplevel() else {
        return false;
    };
    with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|d| d.lock().unwrap().app_id.clone())
            .is_some_and(|id| id.eq_ignore_ascii_case("gamescope"))
    })
}

impl State {
    /// Frames of windows and panels that are completely hidden behind the
    /// ones stacked above them (by any combination of them). Gamescope doesn't
    /// count as covering anything, so a game doesn't get outlines drawn over it.
    fn hidden_frames(&self) -> Vec<Rectangle<i32, Logical>> {
        let mut above: Vec<Rectangle<i32, Logical>> = Vec::new();
        let mut hidden = Vec::new();
        for item in self.stack_top_down() {
            let (content, covers) = match &item {
                StackEntry::Window(w) => (self.space.element_geometry(w), !is_gamescope(w)),
                StackEntry::Panel(id) => (Some(self.panels.content_rect(*id)), true),
            };
            let Some(content) = content else { continue };
            let frame = frame_rect(content);
            if !above.is_empty() && frame.subtract_rects(above.iter().copied()).is_empty() {
                hidden.push(frame);
            }
            if covers {
                above.push(frame);
            }
        }
        hidden
    }

    /// Build all render elements, front-most first.
    pub fn render_elements(&mut self, renderer: &mut GlesRenderer) -> Vec<CustomElement> {
        self.build_elements(renderer, false)
    }

    /// Render elements, front-most first. For a screenshot `capture` leaves
    /// out the cursor, hover highlights, the selection overlay and the toast.
    pub fn build_elements(
        &mut self,
        renderer: &mut GlesRenderer,
        capture: bool,
    ) -> Vec<CustomElement> {
        let mut elems: Vec<CustomElement> = Vec::new();
        let ptr = self.pointer_location;
        let ptr_i = ptr.to_i32_round::<i32>();
        let out = self.output_geometry();
        let selecting = self.ui.shot.is_some();
        let hovered = if capture || selecting {
            Hit::Background
        } else {
            self.hit_test(ptr)
        };
        let focused = self.focused_window();
        let hidden = self.hidden_frames();

        if !capture {
            if let Some(sel) = &self.ui.shot {
                let rect = sel.rect(ptr);
                screenshot_overlay(&mut self.ui.shot_bufs, out, ptr_i, rect, &mut elems);
            } else {
                self.cursor_elements(renderer, ptr_i, &mut elems);
            }
            let ui = &mut self.ui;
            if let Some((msg, since)) = ui.toast.as_ref() {
                if since.elapsed() < TOAST_TIME {
                    let w = ui.text.measure(msg) + 2 * TOAST_PAD;
                    let r = Rectangle::new(
                        (
                            out.loc.x + (out.size.w - w) / 2,
                            out.loc.y + BAR_HEIGHT + 12,
                        )
                            .into(),
                        (w, TOAST_H).into(),
                    );
                    let msg = msg.clone();
                    elems.extend(text_centered(&mut ui.text, renderer, &msg, r));
                    elems.push(solid(&mut ui.toast_bufs[0], r, TOAST_BG));
                    let edge = Rectangle::new(r.loc - Point::from((1, 1)), r.size + (2, 2).into());
                    elems.push(solid(&mut ui.toast_bufs[1], edge, TOAST_EDGE));
                } else {
                    ui.toast = None;
                }
            }
        }

        self.scene_elements(renderer, out, hovered, focused, hidden, &mut elems);
        elems
    }

    fn cursor_elements(
        &mut self,
        renderer: &mut GlesRenderer,
        ptr_i: Point<i32, Logical>,
        elems: &mut Vec<CustomElement>,
    ) {
        let cursor_status = self.cursor_status.clone();
        match cursor_status {
            CursorImageStatus::Hidden => {}
            CursorImageStatus::Surface(surface) if surface.is_alive() => {
                let hotspot = with_states(&surface, |states| {
                    states
                        .data_map
                        .get::<CursorImageSurfaceData>()
                        .map(|d| d.lock().unwrap().hotspot)
                        .unwrap_or_default()
                });
                let pos = (ptr_i - hotspot).to_physical(1);
                elems.extend(render_elements_from_surface_tree(
                    renderer,
                    &surface,
                    pos,
                    Scale::from(1.0),
                    1.0,
                    Kind::Cursor,
                ));
            }
            _ => {
                if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    ptr_i.to_f64().to_physical(1.0),
                    &self.ui.cursor,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                ) {
                    elems.push(e.into());
                }
            }
        }
    }

    /// Menus, bar, windows and panels, front-most first.
    fn scene_elements(
        &mut self,
        renderer: &mut GlesRenderer,
        out: Rectangle<i32, Logical>,
        hovered: Hit,
        focused: Option<Window>,
        hidden: Vec<Rectangle<i32, Logical>>,
        elems: &mut Vec<CustomElement>,
    ) {
        let ui = &mut self.ui;

        // --- open menu ------------------------------------------------------
        if let Some(menu) = &ui.menu {
            let hl = match hovered {
                Hit::Menu(Some(i)) => Some(i),
                _ => None,
            };
            if let MenuKind::Size(_) = menu.kind {
                if let Some(i) = hl {
                    let label = SIZE_OPTIONS[i].0;
                    elems.extend(text_centered(
                        &mut ui.text,
                        renderer,
                        label,
                        menu.label_rect(),
                    ));
                }
                for (i, (_, region)) in SIZE_OPTIONS.iter().enumerate() {
                    let (screen, fill) = size_icon(menu.item_rect(i), *region);
                    let fill_color = if hl == Some(i) {
                        ICON_FILL_HOT
                    } else {
                        ICON_FILL
                    };
                    elems.push(solid(&mut ui.icon_fills[i], fill, fill_color));
                    elems.push(solid(&mut ui.icon_screens[i], screen, ICON_SCREEN));
                }
            } else {
                let empty = matches!(&menu.kind, MenuKind::Launcher(p) if p.is_empty());
                let alpha = if empty { 0.5 } else { 1.0 };
                for (i, label) in menu.kind.items().iter().enumerate() {
                    let r = menu.item_rect(i);
                    elems.extend(text_in_alpha(
                        &mut ui.text,
                        renderer,
                        label,
                        r.loc.x + BAR_BUTTON_PAD,
                        r,
                        alpha,
                    ));
                }
            }
            let empty = matches!(&menu.kind, MenuKind::Launcher(p) if p.is_empty());
            if let Some(i) = hl.filter(|_| !empty) {
                elems.push(solid(&mut ui.menu_hl, menu.item_rect(i), MENU_HL));
            }
            elems.push(solid(&mut ui.menu_bg, menu.rect, MENU_BG));
        }

        // --- outlines of fully hidden windows, above all windows ------------
        ui.hidden_outlines
            .resize_with(hidden.len() * 4, solid_buffer);
        let edges = hidden.into_iter().flat_map(outline);
        for (buf, edge) in ui.hidden_outlines.iter_mut().zip(edges) {
            elems.push(solid(buf, edge, HIDDEN_OUTLINE));
        }

        // --- top bar --------------------------------------------------------
        let open_bar = match ui.menu.as_ref().map(|m| &m.kind) {
            Some(MenuKind::Launcher(_)) => Some(BarButton::Launcher),
            _ => None,
        };
        let (buttons, clock_x) = bar_layout(&ui.text, out);
        for (i, (b, r)) in buttons.into_iter().enumerate() {
            elems.extend(text_centered(&mut ui.text, renderer, b.label(), r));
            let hot = open_bar == Some(b) || matches!(hovered, Hit::BarButton(h) if h == b);
            let color = if hot { BAR_BUTTON_HOT } else { BAR_BUTTON };
            elems.push(solid(&mut ui.bar_buttons[i], r, color));
        }
        let bar = Rectangle::new(out.loc, (out.size.w, BAR_HEIGHT).into());
        // The text cache keeps the clock cheap; it only re-rasterizes when
        // the minute changes.
        let clock = clock_text();
        elems.extend(text_in(&mut ui.text, renderer, &clock, clock_x, bar));
        elems.push(solid(&mut ui.bar_bg, bar, BAR_COLOR));

        // --- windows and the Control Panel (top to bottom) ------------------
        for item in self.stack_top_down() {
            let window = match item {
                StackEntry::Panel(id) => {
                    let hover = match hovered {
                        Hit::Panel(hid, h) if hid == id => Some(h),
                        _ => None,
                    };
                    self.panels
                        .render(id, &mut self.ui.text, renderer, hover, elems);
                    continue;
                }
                StackEntry::Window(w) => w,
            };
            let Some(content) = self.space.element_geometry(&window) else {
                continue;
            };
            let render_loc = content.loc - window.geometry().loc;
            elems.extend(window.render_elements::<CustomElement>(
                renderer,
                render_loc.to_physical(1),
                Scale::from(1.0),
                1.0,
            ));

            let ui = &mut self.ui;
            let frame = Frame::new(content, &ui.text);
            let title = window_title(&window);
            let title = ui.text.fit(&title, frame.title_text_max);
            elems.extend(text_in(
                &mut ui.text,
                renderer,
                &title,
                frame.title_text_x,
                frame.title,
            ));
            elems.extend(text_centered(&mut ui.text, renderer, "Size", frame.size));
            elems.extend(text_centered(&mut ui.text, renderer, "×", frame.close));

            let is_focused = focused.as_ref() == Some(&window);
            let title_color = if is_focused {
                TITLE_FOCUSED
            } else {
                TITLE_UNFOCUSED
            };
            let close_hot = matches!(&hovered, Hit::Close(w) if *w == window);
            let size_hot = matches!(&hovered, Hit::SizeButton(w) if *w == window);

            window
                .user_data()
                .insert_if_missing(|| RefCell::new(DecoBuffers::default()));
            let mut deco = window
                .user_data()
                .get::<RefCell<DecoBuffers>>()
                .unwrap()
                .borrow_mut();
            elems.push(solid(
                &mut deco.close,
                frame.close,
                if close_hot { CLOSE_HOT } else { CLOSE_COLOR },
            ));
            elems.push(solid(
                &mut deco.size,
                frame.size,
                if size_hot { SIZE_HOT } else { SIZE_COLOR },
            ));
            elems.push(solid(&mut deco.title, frame.title, title_color));
            elems.push(solid(&mut deco.left, frame.left, title_color));
            elems.push(solid(&mut deco.right, frame.right, title_color));
            elems.push(solid(&mut deco.bottom, frame.bottom, title_color));
        }
    }
}

/// Screenshot selection overlay, front-most first: a crosshair cursor, the
/// selection's edges, and the dimmed rest of the screen.
fn screenshot_overlay(
    bufs: &mut [SolidColorBuffer],
    out: Rectangle<i32, Logical>,
    ptr: Point<i32, Logical>,
    sel: Option<Rectangle<i32, Logical>>,
    elems: &mut Vec<CustomElement>,
) {
    let rect =
        |x: i32, y: i32, w: i32, h: i32| Rectangle::new((x, y).into(), (w.max(0), h.max(0)).into());
    let (cross, rest) = bufs.split_at_mut(4);
    let (dim, edges) = rest.split_at_mut(4);

    let (px, py, a) = (ptr.x, ptr.y, CROSS_ARM);
    elems.push(solid(
        &mut cross[0],
        rect(px - a, py, 2 * a + 1, 1),
        SHOT_EDGE,
    ));
    elems.push(solid(
        &mut cross[1],
        rect(px, py - a, 1, 2 * a + 1),
        SHOT_EDGE,
    ));
    elems.push(solid(
        &mut cross[2],
        rect(px - a - 1, py - 1, 2 * a + 3, 3),
        CROSS_DARK,
    ));
    elems.push(solid(
        &mut cross[3],
        rect(px - 1, py - a - 1, 3, 2 * a + 3),
        CROSS_DARK,
    ));

    let Some(s) = sel else {
        elems.push(solid(&mut dim[0], out, SHOT_DIM));
        return;
    };
    let (sx, sy, sw, sh) = (s.loc.x, s.loc.y, s.size.w, s.size.h);
    // 1px edges just outside the selection, so it shows exactly what's saved.
    elems.push(solid(
        &mut edges[0],
        rect(sx - 1, sy - 1, sw + 2, 1),
        SHOT_EDGE,
    ));
    elems.push(solid(
        &mut edges[1],
        rect(sx - 1, sy + sh, sw + 2, 1),
        SHOT_EDGE,
    ));
    elems.push(solid(&mut edges[2], rect(sx - 1, sy, 1, sh), SHOT_EDGE));
    elems.push(solid(&mut edges[3], rect(sx + sw, sy, 1, sh), SHOT_EDGE));

    let (ox, oy, ow, oh) = (out.loc.x, out.loc.y, out.size.w, out.size.h);
    elems.push(solid(&mut dim[0], rect(ox, oy, ow, sy - oy), SHOT_DIM));
    elems.push(solid(
        &mut dim[1],
        rect(ox, sy + sh, ow, oy + oh - (sy + sh)),
        SHOT_DIM,
    ));
    elems.push(solid(&mut dim[2], rect(ox, sy, sx - ox, sh), SHOT_DIM));
    elems.push(solid(
        &mut dim[3],
        rect(sx + sw, sy, ox + ow - (sx + sw), sh),
        SHOT_DIM,
    ));
}
