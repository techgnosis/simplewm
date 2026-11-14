//! The services simplewm runs (see `crate::services`) on screen: the service row the
//! Sound and D-Bus applets share (name, status, Start/Stop and Log buttons), the D-Bus
//! applet (the session bus simplewm serves and who's connected to it, see `crate::bus`),
//! and the Log window that shows one service's output.

use smithay::{
    backend::renderer::{element::solid::SolidColorBuffer, gles::GlesRenderer},
    utils::{Logical, Point, Rectangle},
};

use super::{
    BTN_H, HEADER_H, PAD, PanelHit, PanelId, Rect, SEPARATOR, bufs, button_color,
    rows::{ENV_ROW_H, EnvRow, RowList},
};
use crate::{
    services::{self, Service, ServiceEvent},
    text::TextRenderer,
    ui::{self, CustomElement},
};

// --- Layout (logical pixels) -----------------------------------------------

pub(super) const SERVICE_ROW_H: i32 = BTN_H + 6;
const NAME_W: i32 = 130;
const TOGGLE_W: i32 = 64;
const LOG_W: i32 = 52;
const GAP: i32 = 8;

const BUS_W: i32 = 640;
const BUS_LINE_H: i32 = 22;
/// Rows of connected programs shown at once (the list scrolls).
const BUS_LIST_ROWS: i32 = 12;

pub(super) const LOG_WIN_W: i32 = 860;
pub(super) const LOG_WIN_H: i32 = 500;

/// One service's row: its name, status, and the Start/Stop and Log buttons.
pub(super) struct ServiceRow {
    name: Rect,
    status: Rect,
    toggle: Rect,
    log: Rect,
}

pub(super) fn service_row(x: i32, y: i32, w: i32) -> ServiceRow {
    let log = Rectangle::new((x + w - LOG_W, y).into(), (LOG_W, BTN_H).into());
    let toggle = Rectangle::new(
        (log.loc.x - GAP - TOGGLE_W, y).into(),
        (TOGGLE_W, BTN_H).into(),
    );
    let name = Rectangle::new((x, y).into(), (NAME_W, BTN_H).into());
    let status_x = x + NAME_W + GAP;
    let status = Rectangle::new(
        (status_x, y).into(),
        (toggle.loc.x - GAP - status_x, BTN_H).into(),
    );
    ServiceRow {
        name,
        status,
        toggle,
        log,
    }
}

struct BusLayout {
    header: Rect,
    row: ServiceRow,
    separator: Rect,
    address: Rect,
    list: Rect,
}

fn bus_layout(c: Rect) -> BusLayout {
    let x = c.loc.x + PAD;
    let w = c.size.w - 2 * PAD;
    let mut y = c.loc.y + PAD;
    let header = Rectangle::new((x, y).into(), (w, HEADER_H).into());
    y += HEADER_H;
    let row = service_row(x, y, w);
    y += SERVICE_ROW_H;
    y += PAD;
    let separator = Rectangle::new((x, y).into(), (w, 1).into());
    y += 1 + PAD;
    let address = Rectangle::new((x, y).into(), (w, BUS_LINE_H).into());
    y += BUS_LINE_H + 4;
    let list = Rectangle::new((x, y).into(), (w, BUS_LIST_ROWS * ENV_ROW_H).into());
    BusLayout {
        header,
        row,
        separator,
        address,
        list,
    }
}

impl super::Panels {
    // --- the shared service row ---------------------------------------------

    pub(super) fn service_row_hit(
        &self,
        s: Service,
        row: &ServiceRow,
        p: Point<f64, Logical>,
    ) -> Option<PanelHit> {
        if self.services.can_start(s).is_some() && row.toggle.to_f64().contains(p) {
            return Some(PanelHit::ServiceToggle(s));
        }
        row.log
            .to_f64()
            .contains(p)
            .then_some(PanelHit::ServiceLog(s))
    }

    /// Draw a service row. Uses two buffers from `solids` (Start/Stop, Log). Takes the
    /// services rather than `&self`, so callers can hold their own window's buffers.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_service_row(
        services: &services::Services,
        s: Service,
        row: &ServiceRow,
        solids: &mut [SolidColorBuffer],
        text: &mut TextRenderer,
        renderer: &mut GlesRenderer,
        hovered: Option<PanelHit>,
        elems: &mut Vec<CustomElement>,
    ) {
        elems.extend(ui::text_in(
            text,
            renderer,
            s.name(),
            row.name.loc.x,
            row.name,
        ));
        let status = text.fit(&services.describe(s), row.status.size.w);
        elems.extend(ui::text_in_alpha(
            text,
            renderer,
            &status,
            row.status.loc.x,
            row.status,
            0.8,
        ));
        let color = |hit| button_color(hovered, hit);
        if let Some(start) = services.can_start(s) {
            let label = if start { "Start" } else { "Stop" };
            elems.extend(ui::text_centered(text, renderer, label, row.toggle));
            elems.push(ui::solid(
                &mut solids[0],
                row.toggle,
                color(PanelHit::ServiceToggle(s)),
            ));
        }
        elems.extend(ui::text_centered(text, renderer, "Log", row.log));
        elems.push(ui::solid(
            &mut solids[1],
            row.log,
            color(PanelHit::ServiceLog(s)),
        ));
    }

    /// The Start/Stop button.
    pub fn service_toggle(&mut self, s: Service) {
        match self.services.can_start(s) {
            Some(true) => self.services.start(s),
            Some(false) => self.services.stop(s),
            None => {}
        }
        self.after_service_change(s);
    }

    /// An event from a service's threads: update its status and log, and whatever
    /// window shows them.
    pub fn service_event(&mut self, ev: ServiceEvent) {
        let s = ev.service;
        let changed = self.services.handle(ev);
        if self.log_service == Some(s) && self.is_open(PanelId::Log) {
            self.fill_log();
        }
        if changed {
            self.after_service_change(s);
        }
    }

    /// Refresh what depends on service `s`: the Sound applet's cards and volume.
    fn after_service_change(&mut self, s: Service) {
        if s != Service::Bus && self.is_open(PanelId::Sound) {
            self.refresh_sound();
        }
    }

    // --- D-Bus applet ---------------------------------------------------------

    pub(super) fn bus_size(&self) -> (i32, i32) {
        let c = bus_layout(Rectangle::new((0, 0).into(), (BUS_W, 0).into()));
        (BUS_W, c.list.loc.y + c.list.size.h + PAD)
    }

    pub(super) fn bus_hit(&self, p: Point<f64, Logical>) -> Option<PanelHit> {
        let l = bus_layout(self.content_rect(PanelId::Bus));
        self.service_row_hit(Service::Bus, &l.row, p)
    }

    /// The connected programs, or why there's no list.
    fn bus_rows(&self) -> Vec<EnvRow> {
        let Some(peers) = self.services.bus_peers() else {
            let why = match self.services.status(Service::Bus) {
                services::Status::Elsewhere(_) => {
                    "Another program runs the bus, so simplewm can't list who's connected."
                }
                _ => "The bus isn't running.",
            };
            return vec![EnvRow::Line(why.into())];
        };
        let mut rows = vec![EnvRow::Header(format!(
            "Connected programs: {}",
            peers.len()
        ))];
        rows.extend(peers.into_iter().map(|p| {
            let program = std::path::Path::new(&p.program)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let who = if program.is_empty() {
                format!("{}  PID {}", p.name, p.pid)
            } else {
                format!("{}  {program} ({})", p.name, p.pid)
            };
            EnvRow::Item(who, p.names.join(", "))
        }));
        rows
    }

    pub(super) fn scroll_bus(&mut self, amount: f64) {
        let vp = bus_layout(self.content_rect(PanelId::Bus)).list;
        self.bus_list.scroll_by(amount, vp);
    }

    pub(super) fn render_bus(
        &mut self,
        text: &mut TextRenderer,
        renderer: &mut GlesRenderer,
        hovered: Option<PanelHit>,
        elems: &mut Vec<CustomElement>,
    ) {
        let l = bus_layout(self.content_rect(PanelId::Bus));
        let rows = self.bus_rows();
        self.bus_list.set(rows, l.list);
        // Buffers: Start/Stop, Log, separator.
        let solids = bufs(&mut self.bus_solids, 3);
        elems.extend(ui::text_in(
            text,
            renderer,
            "Session bus",
            l.header.loc.x,
            l.header,
        ));
        Self::render_service_row(
            &self.services,
            Service::Bus,
            &l.row,
            &mut solids[0..2],
            text,
            renderer,
            hovered,
            elems,
        );
        let address = services::bus_address().unwrap_or_else(|| "none".into());
        let address = text.fit(&format!("Address: {address}"), l.address.size.w);
        elems.extend(ui::text_in(
            text,
            renderer,
            &address,
            l.address.loc.x,
            l.address,
        ));
        elems.push(ui::solid(&mut solids[2], l.separator, SEPARATOR));
        self.bus_list.render(
            l.list,
            &mut self.windows[PanelId::Bus as usize].solids,
            text,
            renderer,
            elems,
        );
    }

    // --- Log window -------------------------------------------------------------

    /// The Log button: show service `s`'s log in the Log window.
    pub fn open_log(&mut self, s: Service) {
        self.log_service = Some(s);
        self.log_title = format!("Log: {}", s.name());
        self.log_rows = RowList::default();
        self.fill_log();
        self.log_rows.scroll_to_end(self.log_viewport());
        self.open_request = Some(PanelId::Log);
    }

    /// Put the service's log into the Log window, following new lines if it was
    /// scrolled to the end.
    fn fill_log(&mut self) {
        let Some(s) = self.log_service else { return };
        let log = self.services.log(s);
        let rows = if log.is_empty() {
            let why = match self.services.status(s) {
                services::Status::Elsewhere(who) => {
                    format!("{who} runs {}, so its output goes there.", s.program())
                }
                _ => format!("{} hasn't been started by simplewm yet.", s.program()),
            };
            vec![EnvRow::Line(why)]
        } else {
            log.iter().map(|l| EnvRow::Line(l.clone())).collect()
        };
        let vp = self.log_viewport();
        self.log_rows.set_following(rows, vp);
    }

    pub(super) fn log_viewport(&self) -> Rect {
        let c = self.content_rect(PanelId::Log);
        Rectangle::new(
            (c.loc.x + PAD, c.loc.y + PAD).into(),
            (c.size.w - 2 * PAD, c.size.h - 2 * PAD).into(),
        )
    }

    pub(super) fn render_log(
        &mut self,
        text: &mut TextRenderer,
        renderer: &mut GlesRenderer,
        elems: &mut Vec<CustomElement>,
    ) {
        let vp = self.log_viewport();
        self.log_rows.render(
            vp,
            &mut self.windows[PanelId::Log as usize].solids,
            text,
            renderer,
            elems,
        );
    }
}
