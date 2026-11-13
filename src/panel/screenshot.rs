//! The Screenshot applet: the folder screenshots are saved in (see
//! `crate::screenshot`), chosen with the File Picker. There is no default.

use std::path::{Path, PathBuf};

use smithay::{
    backend::renderer::gles::GlesRenderer,
    utils::{Logical, Point, Rectangle},
};

use super::{
    BUTTON, BUTTON_HOT, FIELD_BG, FIELD_H, FIELD_TEXT_PAD, HEADER_H, NOTE_H, PAD, PanelHit,
    PanelId, Rect, bufs,
};
use crate::{
    text::TextRenderer,
    ui::{self, CustomElement},
};

// --- Layout (logical pixels) -----------------------------------------------

const SHOT_W: i32 = 520;
const CHOOSE_W: i32 = 90;

/// The settings key the folder is stored under.
pub(super) const SHOT_DIR_KEY: &str = "screenshot_dir";

struct ScreenshotLayout {
    header: Rect,
    /// The folder, shown read-only.
    path: Rect,
    choose: Rect,
    note: Rect,
}

/// `dir` with a leading `~` expanded (older settings may start with one), or
/// None when it is empty.
fn expand_dir(dir: &str, home: Option<PathBuf>) -> Option<PathBuf> {
    let dir = dir.trim();
    if dir.is_empty() {
        return None;
    }
    if dir == "~" {
        return home;
    }
    if let Some(rest) = dir.strip_prefix("~/") {
        return home.map(|h| h.join(rest));
    }
    Some(PathBuf::from(dir))
}

impl super::Panels {
    pub(super) fn screenshot_size(&self) -> (i32, i32) {
        (SHOT_W, PAD + HEADER_H + FIELD_H + NOTE_H + PAD)
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
            choose: Rectangle::new(
                (x + path_w + 8, y + HEADER_H).into(),
                (CHOOSE_W, FIELD_H).into(),
            ),
            note: Rectangle::new((x, y + HEADER_H + FIELD_H).into(), (w, NOTE_H).into()),
        }
    }

    pub(super) fn screenshot_hit(&self, p: Point<f64, Logical>) -> Option<PanelHit> {
        self.screenshot_layout()
            .choose
            .to_f64()
            .contains(p)
            .then_some(PanelHit::ChooseShotDir)
    }

    /// Folder to save screenshots in, if one is set (`~` is expanded).
    pub fn screenshot_dir(&self) -> Option<PathBuf> {
        expand_dir(&self.shot_dir, std::env::var_os("HOME").map(PathBuf::from))
    }

    /// Set the screenshot folder (chosen in the File Picker) and save it.
    pub fn set_screenshot_dir(&mut self, dir: &Path) {
        self.shot_dir = dir.display().to_string();
        let value = self.shot_dir.trim().to_string();
        self.save_settings(&[(SHOT_DIR_KEY, &value)]);
    }

    pub(super) fn render_screenshot(
        &mut self,
        text: &mut TextRenderer,
        renderer: &mut GlesRenderer,
        hovered: Option<PanelHit>,
        elems: &mut Vec<CustomElement>,
    ) {
        let l = self.screenshot_layout();
        // Buffers: path box, Choose button.
        let s = bufs(&mut self.screenshot.solids, 2);

        elems.extend(ui::text_in(
            text,
            renderer,
            "Folder where screenshots are saved",
            l.header.loc.x,
            l.header,
        ));
        let tx = l.path.loc.x + FIELD_TEXT_PAD;
        let max = l.path.size.w - 2 * FIELD_TEXT_PAD;
        if self.shot_dir.trim().is_empty() {
            elems.extend(ui::text_in_alpha(
                text,
                renderer,
                "No folder chosen",
                tx,
                l.path,
                0.35,
            ));
        } else {
            let shown = text.fit(&self.shot_dir, max);
            elems.extend(ui::text_in(text, renderer, &shown, tx, l.path));
        }
        elems.extend(ui::text_centered(text, renderer, "Choose…", l.choose));
        let note = "Files are named simplewm-screenshot-<date>-<time>.png";
        elems.extend(ui::text_in_alpha(
            text,
            renderer,
            note,
            l.note.loc.x,
            l.note,
            0.6,
        ));

        elems.push(ui::solid(&mut s[0], l.path, FIELD_BG));
        let cc = if hovered == Some(PanelHit::ChooseShotDir) {
            BUTTON_HOT
        } else {
            BUTTON
        };
        elems.push(ui::solid(&mut s[1], l.choose, cc));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_home() {
        let home = || Some(PathBuf::from("/home/u"));
        assert_eq!(expand_dir("", home()), None);
        assert_eq!(expand_dir("  ", home()), None);
        assert_eq!(expand_dir("~", home()), home());
        assert_eq!(
            expand_dir("~/Pictures", home()),
            Some("/home/u/Pictures".into())
        );
        assert_eq!(expand_dir("/tmp/shots ", home()), Some("/tmp/shots".into()));
        assert_eq!(expand_dir("~/x", None), None);
    }
}
