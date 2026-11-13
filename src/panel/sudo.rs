//! The Password window: asks for the user's password when something needs
//! root, then does that through sudo on a worker thread. Today the only such
//! thing is setting the network address and DNS server
//! (`crate::network::apply`). The window
//! is not in the Control Panel; it opens when asked for, focused.
//!
//! The password lives in `SudoPrompt::password` only while it is typed. It is
//! moved to the worker on OK and wiped there after use, and wiped here on
//! Cancel.

use smithay::{
    backend::renderer::{element::solid::SolidColorBuffer, gles::GlesRenderer},
    reexports::calloop::channel::Sender,
    utils::{Logical, Point, Rectangle},
};

use super::{
    BTN_H, BUTTON, BUTTON_HOT, FIELD_H, Field, HEADER_H, NOTE_H, PAD, PanelHit, PanelId, Rect,
    bufs, field_char_at, render_field,
};
use crate::{
    network::{self, ApplyError, Ipv4Config},
    text::TextRenderer,
    ui::{self, CustomElement},
};

// --- Layout (logical pixels) -----------------------------------------------

const SUDO_W: i32 = 460;
const BUTTON_W: i32 = 90;

/// Something to do as root.
#[derive(Debug, Clone)]
pub enum SudoJob {
    /// Set a static address on a wired card.
    Network { dev: String, cfg: Ipv4Config },
}

impl SudoJob {
    /// Why the password is needed.
    fn reason(&self) -> String {
        match self {
            SudoJob::Network { dev, cfg } if cfg.dns.is_some() => {
                format!("Setting the network on {dev} and the DNS server needs your password.")
            }
            SudoJob::Network { dev, .. } => {
                format!("Setting the network address on {dev} needs your password.")
            }
        }
    }

    /// Do it (on a worker thread).
    fn run(&self, password: String) -> Result<(), ApplyError> {
        match self {
            SudoJob::Network { dev, cfg } => network::apply(dev, cfg, password),
        }
    }
}

/// A job's outcome, sent back from the worker thread.
pub struct SudoDone {
    result: Result<(), ApplyError>,
}

#[derive(Default)]
pub struct SudoPrompt {
    /// What the password is for (None once done or cancelled).
    job: Option<SudoJob>,
    pub password: String,
    /// Waiting for sudo.
    busy: bool,
    /// Why the last try failed.
    error: Option<String>,
    /// Where workers send `SudoDone` (set up by `State::new`).
    pub tx: Option<Sender<SudoDone>>,
    solids: Vec<SolidColorBuffer>,
}

struct SudoLayout {
    reason: Rect,
    field: Rect,
    note: Rect,
    cancel: Rect,
    ok: Rect,
}

fn layout(c: Rect) -> SudoLayout {
    let x = c.loc.x + PAD;
    let w = c.size.w - 2 * PAD;
    let mut y = c.loc.y + PAD;
    let reason = Rectangle::new((x, y).into(), (w, HEADER_H).into());
    y += HEADER_H;
    let field = Rectangle::new((x, y).into(), (w, FIELD_H).into());
    y += FIELD_H + 4;
    let note = Rectangle::new((x, y).into(), (w, NOTE_H).into());
    y += NOTE_H + 4;
    let ok = Rectangle::new((x + w - BUTTON_W, y).into(), (BUTTON_W, BTN_H).into());
    let cancel = Rectangle::new(
        (ok.loc.x - 8 - BUTTON_W, y).into(),
        (BUTTON_W, BTN_H).into(),
    );
    SudoLayout {
        reason,
        field,
        note,
        cancel,
        ok,
    }
}

/// What a password field shows: one dot per character.
fn masked(password: &str) -> String {
    "•".repeat(password.chars().count())
}

impl super::Panels {
    pub(super) fn sudo_size(&self) -> (i32, i32) {
        (
            SUDO_W,
            PAD + HEADER_H + FIELD_H + 4 + NOTE_H + 4 + BTN_H + PAD,
        )
    }

    fn sudo_layout(&self) -> SudoLayout {
        layout(self.content_rect(PanelId::Password))
    }

    /// Ask for the password to do `job`, opening the Password window.
    pub(super) fn ask_password(&mut self, job: SudoJob) {
        let s = &mut self.sudo;
        if s.busy {
            return;
        }
        network::wipe(&mut s.password);
        s.job = Some(job);
        s.error = None;
        self.open_request = Some(PanelId::Password);
        self.begin_edit(Field::SudoPassword, 0);
    }

    /// OK (or Enter): try the password.
    pub fn sudo_submit(&mut self) {
        let s = &mut self.sudo;
        let (Some(job), Some(tx)) = (s.job.clone(), s.tx.clone()) else {
            return;
        };
        if s.busy || s.password.is_empty() {
            return;
        }
        s.busy = true;
        s.error = None;
        let password = std::mem::take(&mut s.password);
        self.stop_editing();
        let spawned = std::thread::Builder::new()
            .name("sudo".into())
            .spawn(move || {
                let result = job.run(password);
                let _ = tx.send(SudoDone { result });
            });
        if let Err(err) = spawned {
            self.sudo_done(SudoDone {
                result: Err(ApplyError::Failed(format!("cannot start a thread: {err}"))),
            });
        }
    }

    /// Cancel, Esc or the close button.
    pub fn sudo_cancel(&mut self) {
        if self.sudo.busy {
            // sudo is already running with the password; let it finish.
            return;
        }
        network::wipe(&mut self.sudo.password);
        if let Some(job) = self.sudo.job.take() {
            self.job_finished(&job, Err("cancelled".into()));
        }
        self.close(PanelId::Password);
    }

    /// A worker finished. Returns whether the Password window closed.
    pub fn sudo_done(&mut self, done: SudoDone) -> bool {
        self.sudo.busy = false;
        let Some(job) = self.sudo.job.clone() else {
            return false;
        };
        match done.result {
            Err(ApplyError::WrongPassword) => {
                self.sudo.error = Some("Wrong password. Try again.".into());
                self.begin_edit(Field::SudoPassword, 0);
                false
            }
            result => {
                self.sudo.job = None;
                let result = result.map_err(|e| match e {
                    ApplyError::Failed(msg) => msg,
                    ApplyError::WrongPassword => unreachable!(),
                });
                self.job_finished(&job, result);
                self.close(PanelId::Password);
                true
            }
        }
    }

    /// Tell the applet that asked how its job went.
    fn job_finished(&mut self, job: &SudoJob, result: Result<(), String>) {
        match job {
            SudoJob::Network { dev, cfg } => self.network_applied(dev, cfg.dns.is_some(), result),
        }
    }

    pub(super) fn sudo_hit(&self, text: &TextRenderer, p: Point<f64, Logical>) -> Option<PanelHit> {
        let l = self.sudo_layout();
        let inside = |r: Rect| r.to_f64().contains(p);
        // While sudo is checking, there is nothing to type or press.
        if self.sudo.busy {
            return None;
        }
        if inside(l.field) {
            let cursor = (self.editing == Some(Field::SudoPassword)).then_some(self.cursor);
            let shown = masked(&self.sudo.password);
            return Some(PanelHit::Field(
                Field::SudoPassword,
                field_char_at(text, &shown, cursor, l.field, p.x),
            ));
        }
        if inside(l.ok) {
            return Some(PanelHit::SudoOk);
        }
        inside(l.cancel).then_some(PanelHit::SudoCancel)
    }

    pub(super) fn render_sudo(
        &mut self,
        text: &mut TextRenderer,
        renderer: &mut GlesRenderer,
        hovered: Option<PanelHit>,
        elems: &mut Vec<CustomElement>,
    ) {
        let l = self.sudo_layout();
        let cursor = (self.editing == Some(Field::SudoPassword)).then_some(self.cursor);
        let s = &mut self.sudo;
        // Buffers: 3 for the field, Cancel, OK.
        let solids = bufs(&mut s.solids, 5);

        let reason = s.job.as_ref().map(SudoJob::reason).unwrap_or_default();
        let reason = text.fit(&reason, l.reason.size.w);
        elems.extend(ui::text_in(
            text,
            renderer,
            &reason,
            l.reason.loc.x,
            l.reason,
        ));
        let shown = masked(&s.password);
        render_field(
            text,
            renderer,
            &mut solids[0..3],
            l.field,
            &shown,
            "Password",
            cursor,
            elems,
        );
        let (note, alpha) = match (&s.error, s.busy) {
            (_, true) => ("Checking…", 0.6),
            (Some(err), false) => (err.as_str(), 1.0),
            (None, false) => ("Passed to sudo and then forgotten", 0.45),
        };
        elems.extend(ui::text_in_alpha(
            text,
            renderer,
            note,
            l.note.loc.x,
            l.note,
            alpha,
        ));

        elems.extend(ui::text_centered(text, renderer, "Cancel", l.cancel));
        elems.extend(ui::text_centered(text, renderer, "OK", l.ok));
        let busy = s.busy;
        let button = |hit: PanelHit| {
            if hovered == Some(hit) && !busy {
                BUTTON_HOT
            } else {
                BUTTON
            }
        };
        elems.push(ui::solid(
            &mut solids[3],
            l.cancel,
            button(PanelHit::SudoCancel),
        ));
        elems.push(ui::solid(&mut solids[4], l.ok, button(PanelHit::SudoOk)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_by_char() {
        assert_eq!(masked(""), "");
        assert_eq!(masked("pä55"), "••••");
    }
}
