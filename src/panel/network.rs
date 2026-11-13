//! The Network applet: shows the wired card's state and sets a static IPv4
//! address and default gateway on it (see `crate::network`). Apply saves the
//! values and sets them on the card, which needs root: it opens the Password
//! window (see `sudo`). simplewm never sets them on its own, so after a reboot
//! the applet points out that they need applying.

use smithay::{
    backend::renderer::{element::solid::SolidColorBuffer, gles::GlesRenderer},
    utils::{Logical, Point, Rectangle},
};

use super::{
    BTN_H, BUTTON, BUTTON_HOT, ENV_ROW_H, EnvRow, FIELD_H, Field, NOTE_H, PAD, PanelHit, PanelId,
    Rect, RowList, SEPARATOR, bufs, field_char_at, render_field, sudo::SudoJob,
};
use crate::{
    network,
    text::TextRenderer,
    ui::{self, CustomElement},
};

// --- Layout (logical pixels) -----------------------------------------------

const NET_W: i32 = 560;
const STATUS_ROWS: i32 = 9;
const LABEL_W: i32 = 76;
const FIELD_GAP: i32 = 8;
const APPLY_W: i32 = 90;
const REFRESH_W: i32 = 90;

const KEY_ADDRESS: &str = "net_address";
const KEY_GATEWAY: &str = "net_gateway";

#[derive(Default)]
pub struct NetworkApplet {
    /// "192.168.1.50/24"
    pub address: String,
    /// "192.168.1.1", or empty for no default route.
    pub gateway: String,
    /// What is saved: (address, gateway). The fields may differ while edited.
    saved: (String, String),
    status: RowList,
    /// Result of the last Apply: (message, is an error).
    note: Option<(String, bool)>,
    /// Buffers besides the status rows (those use the window's own).
    solids: Vec<SolidColorBuffer>,
}

struct NetLayout {
    status: Rect,
    separator: Rect,
    address_label: Rect,
    address: Rect,
    gateway_label: Rect,
    gateway: Rect,
    note: Rect,
    apply: Rect,
    refresh: Rect,
}

fn layout(c: Rect) -> NetLayout {
    let x = c.loc.x + PAD;
    let w = c.size.w - 2 * PAD;
    let mut y = c.loc.y + PAD;
    let status = Rectangle::new((x, y).into(), (w, STATUS_ROWS * ENV_ROW_H).into());
    y += status.size.h + PAD;
    let separator = Rectangle::new((x, y).into(), (w, 1).into());
    y += 1 + PAD;
    let row = |y: i32| {
        (
            Rectangle::new((x, y).into(), (LABEL_W, FIELD_H).into()),
            Rectangle::new((x + LABEL_W, y).into(), (w - LABEL_W, FIELD_H).into()),
        )
    };
    let (address_label, address) = row(y);
    y += FIELD_H + FIELD_GAP;
    let (gateway_label, gateway) = row(y);
    y += FIELD_H + 4;
    let note = Rectangle::new((x, y).into(), (w, NOTE_H).into());
    y += NOTE_H + 4;
    let apply = Rectangle::new((x, y).into(), (APPLY_W, BTN_H).into());
    let refresh = Rectangle::new((x + APPLY_W + 8, y).into(), (REFRESH_W, BTN_H).into());
    NetLayout {
        status,
        separator,
        address_label,
        address,
        gateway_label,
        gateway,
        note,
        apply,
        refresh,
    }
}

fn content_height() -> i32 {
    PAD + STATUS_ROWS * ENV_ROW_H
        + PAD
        + 1
        + PAD
        + 2 * FIELD_H
        + FIELD_GAP
        + 4
        + NOTE_H
        + 4
        + BTN_H
        + PAD
}

/// Rows describing the card's current state, and whether it has the `saved`
/// settings.
fn status_rows(saved: Option<network::Ipv4Config>) -> Vec<EnvRow> {
    let mut rows = Vec::new();
    if network::network_manager_running() {
        rows.push(EnvRow::Line(
            "NetworkManager is running and may undo these settings.".into(),
        ));
    }
    let Some(dev) = network::ethernet_device() else {
        rows.push(EnvRow::Line("No wired network card found.".into()));
        return rows;
    };
    let item = |k: &str, v: String| EnvRow::Item(k.into(), v);
    rows.push(item("Card", dev.clone()));
    match network::status(&dev) {
        Ok(s) => {
            if let Some(cfg) = saved {
                let has_addr = s.addrs.contains(&(cfg.addr, cfg.prefix));
                let has_gw = cfg.gateway.is_none_or(|g| s.gateways.contains(&g));
                if !(has_addr && has_gw) {
                    rows.insert(
                        0,
                        EnvRow::Line(
                            "The card doesn't have your saved settings. Press Apply to set them."
                                .into(),
                        ),
                    );
                }
            }
            rows.push(item("Hardware address", s.mac));
            let state = if s.up {
                format!("on ({})", s.operstate)
            } else {
                "off".into()
            };
            rows.push(item("Interface", state));
            let cable = match (s.up, s.carrier) {
                (false, _) | (_, None) => "unknown while the interface is off",
                (true, Some(true)) => "plugged in",
                (true, Some(false)) => "unplugged",
            };
            rows.push(item("Cable", cable.into()));
            if let Some(speed) = s.speed.filter(|_| s.carrier == Some(true)) {
                rows.push(item("Speed", format!("{speed} Mb/s")));
            }
            let addrs: Vec<String> = s.addrs.iter().map(|(a, p)| format!("{a}/{p}")).collect();
            rows.push(item(
                "IPv4 address",
                if addrs.is_empty() {
                    "none".into()
                } else {
                    addrs.join(", ")
                },
            ));
            let gws: Vec<String> = s.gateways.iter().map(|g| g.to_string()).collect();
            rows.push(item(
                "Default gateway",
                if gws.is_empty() {
                    "none".into()
                } else {
                    gws.join(", ")
                },
            ));
        }
        Err(err) => rows.push(EnvRow::Line(format!(
            "Could not read the card: {}",
            network::describe(&err)
        ))),
    }
    rows
}

// --- Panels glue ---------------------------------------------------------------

impl super::Panels {
    pub(super) fn load_network(&mut self) {
        if let Some(conn) = &self.db {
            self.net.address = super::db::load_setting(conn, KEY_ADDRESS).unwrap_or_default();
            self.net.gateway = super::db::load_setting(conn, KEY_GATEWAY).unwrap_or_default();
        }
        self.net.saved = (self.net.address.clone(), self.net.gateway.clone());
    }

    /// The saved settings, if there are any and they are valid.
    fn saved_network(&self) -> Option<network::Ipv4Config> {
        let (address, gateway) = &self.net.saved;
        if address.is_empty() {
            return None;
        }
        network::parse(address, gateway).ok()
    }

    pub(super) fn network_size(&self) -> (i32, i32) {
        (NET_W, content_height())
    }

    fn net_layout(&self) -> NetLayout {
        layout(self.content_rect(PanelId::Network))
    }

    /// Re-read the card's state (on open, Refresh and after Apply).
    pub fn refresh_network(&mut self) {
        let vp = self.net_layout().status;
        let saved = self.saved_network();
        self.net.status.set(status_rows(saved), vp);
    }

    pub(super) fn scroll_network(&mut self, amount: f64) {
        let vp = self.net_layout().status;
        self.net.status.scroll_by(amount, vp);
    }

    pub(super) fn network_hit(
        &self,
        text: &TextRenderer,
        p: Point<f64, Logical>,
    ) -> Option<PanelHit> {
        let l = self.net_layout();
        let inside = |r: Rect| r.to_f64().contains(p);
        for (f, rect) in [
            (Field::NetAddress, l.address),
            (Field::NetGateway, l.gateway),
        ] {
            if inside(rect) {
                let cursor = (self.editing == Some(f)).then_some(self.cursor);
                return Some(PanelHit::Field(
                    f,
                    field_char_at(text, self.field_text(f), cursor, rect, p.x),
                ));
            }
        }
        if inside(l.apply) {
            return Some(PanelHit::NetApply);
        }
        if inside(l.refresh) {
            return Some(PanelHit::NetRefresh);
        }
        None
    }

    /// The Apply button (or Enter in a field): save the fields, then ask for
    /// the password to set them on the card.
    pub fn network_apply(&mut self) {
        self.stop_editing();
        let (address, gateway) = (
            self.net.address.trim().to_string(),
            self.net.gateway.trim().to_string(),
        );
        if address.is_empty() && gateway.is_empty() {
            self.save_settings(&[(KEY_ADDRESS, ""), (KEY_GATEWAY, "")]);
            self.net.saved = Default::default();
            self.net.note = Some(("Cleared. The card keeps what it has now.".into(), false));
            self.refresh_network();
            return;
        }
        let cfg = match network::parse(&address, &gateway) {
            Ok(cfg) => cfg,
            Err(err) => {
                self.net.note = Some((err, true));
                return;
            }
        };
        self.net.address = address.clone();
        self.net.gateway = gateway.clone();
        self.save_settings(&[(KEY_ADDRESS, &address), (KEY_GATEWAY, &gateway)]);
        self.net.saved = (address, gateway);
        match network::ethernet_device() {
            None => {
                self.net.note = Some((
                    "Saved, but there is no wired network card to set it on.".into(),
                    true,
                ))
            }
            Some(dev) => {
                self.net.note = Some(("Saved. Waiting for your password…".into(), false));
                self.ask_password(SudoJob::Network { dev, cfg });
            }
        }
        self.refresh_network();
    }

    /// How setting the address on `dev` went (from the Password window).
    pub(super) fn network_applied(&mut self, dev: &str, result: Result<(), String>) {
        self.net.note = Some(match result {
            Ok(()) => (format!("Set on {dev} and saved."), false),
            Err(err) => (format!("Saved, but not set on the card: {err}"), true),
        });
        self.refresh_network();
    }

    pub(super) fn render_network(
        &mut self,
        text: &mut TextRenderer,
        renderer: &mut GlesRenderer,
        hovered: Option<PanelHit>,
        elems: &mut Vec<CustomElement>,
    ) {
        let l = self.net_layout();
        self.net
            .status
            .render(l.status, &mut self.network.solids, text, renderer, elems);

        // Buffers: 3 per field, separator, Apply, Refresh.
        let s = bufs(&mut self.net.solids, 9);
        let fields = [
            (
                Field::NetAddress,
                "Address",
                l.address_label,
                l.address,
                "192.168.1.50/24",
            ),
            (
                Field::NetGateway,
                "Gateway",
                l.gateway_label,
                l.gateway,
                "192.168.1.1 (or empty for none)",
            ),
        ];
        for (k, (f, label, label_rect, rect, placeholder)) in fields.into_iter().enumerate() {
            elems.extend(ui::text_in(
                text,
                renderer,
                label,
                label_rect.loc.x,
                label_rect,
            ));
            let cursor = (self.editing == Some(f)).then_some(self.cursor);
            let value = match f {
                Field::NetAddress => &self.net.address,
                _ => &self.net.gateway,
            };
            render_field(
                text,
                renderer,
                &mut s[3 * k..3 * k + 3],
                rect,
                value,
                placeholder,
                cursor,
                elems,
            );
        }

        let (note, alpha) = match &self.net.note {
            Some((msg, true)) => (msg.as_str(), 1.0),
            Some((msg, false)) => (msg.as_str(), 0.8),
            None => (
                "Apply saves these and asks for your password · Tab switches fields",
                0.45,
            ),
        };
        let note = text.fit(note, l.note.size.w);
        elems.extend(ui::text_in_alpha(
            text,
            renderer,
            &note,
            l.note.loc.x,
            l.note,
            alpha,
        ));

        elems.extend(ui::text_centered(text, renderer, "Apply", l.apply));
        elems.extend(ui::text_centered(text, renderer, "Refresh", l.refresh));
        let button = |hit: PanelHit| {
            if hovered == Some(hit) {
                BUTTON_HOT
            } else {
                BUTTON
            }
        };
        elems.push(ui::solid(&mut s[6], l.separator, SEPARATOR));
        elems.push(ui::solid(&mut s[7], l.apply, button(PanelHit::NetApply)));
        elems.push(ui::solid(
            &mut s[8],
            l.refresh,
            button(PanelHit::NetRefresh),
        ));
    }
}
