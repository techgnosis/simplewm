//! The Launcher applet: the executables listed in the top bar's Launcher
//! menu, each with Change… and Remove buttons, and Add…. Paths are chosen in
//! the File Picker and saved immediately.

use std::path::{Path, PathBuf};

use smithay::{
    backend::renderer::gles::GlesRenderer,
    utils::{Logical, Point, Rectangle},
};

use super::{
    BTN_H, BUTTON, BUTTON_HOT, FIELD_BG, FIELD_H, FIELD_TEXT_PAD, HEADER_H, PAD, PanelHit, PanelId,
    Rect, bufs, db,
};
use crate::{
    text::TextRenderer,
    ui::{self, CustomElement},
};

// --- Layout (logical pixels) -----------------------------------------------

const LAUNCHER_W: i32 = 520;
const ROW_H: i32 = 32;
const CHANGE_W: i32 = 90;
const REMOVE_W: i32 = 76;
const ADD_W: i32 = 90;

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

/// Display name for a launcher entry: the executable's file name.
pub fn exe_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

impl super::Panels {
    pub(super) fn launcher_size(&self) -> (i32, i32) {
        let rows = self.entries.len() as i32;
        (LAUNCHER_W, PAD + HEADER_H + rows * ROW_H + 4 + BTN_H + PAD)
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
                remove: Rectangle::new(
                    (change_x + CHANGE_W + 8, y).into(),
                    (REMOVE_W, FIELD_H).into(),
                ),
            });
            y += ROW_H;
        }
        let add = Rectangle::new((x, y + 4).into(), (ADD_W, BTN_H).into());
        LauncherLayout { header, rows, add }
    }

    pub(super) fn launcher_hit(&self, p: Point<f64, Logical>) -> Option<PanelHit> {
        let l = self.launcher_layout();
        for (i, row) in l.rows.iter().enumerate() {
            if row.change.to_f64().contains(p) {
                return Some(PanelHit::Change(i));
            }
            if row.remove.to_f64().contains(p) {
                return Some(PanelHit::Remove(i));
            }
        }
        l.add.to_f64().contains(p).then_some(PanelHit::Add)
    }

    /// Non-empty launcher paths, in order.
    pub fn launcher_paths(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty())
            .collect()
    }

    /// Add executables chosen in the File Picker (skipping ones already listed).
    pub fn add_entries(&mut self, paths: &[PathBuf]) {
        for p in paths {
            let p = p.display().to_string();
            if !self.entries.contains(&p) {
                self.entries.push(p);
            }
        }
        self.save_entries();
    }

    /// Replace entry `old` with `new` (appended if `old` was removed meanwhile).
    pub fn change_entry(&mut self, old: &str, new: &Path) {
        let new = new.display().to_string();
        match self.entries.iter().position(|e| e == old) {
            Some(i) => self.entries[i] = new,
            None => self.entries.push(new),
        }
        self.save_entries();
    }

    pub fn remove_entry(&mut self, i: usize) {
        if i >= self.entries.len() {
            return;
        }
        self.entries.remove(i);
        self.save_entries();
    }

    fn save_entries(&mut self) {
        let paths = self.launcher_paths();
        if let Some(conn) = self.db.as_mut() {
            db::save_entries(conn, &paths);
        }
    }

    pub(super) fn render_launcher(
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

        elems.extend(ui::text_in(
            text,
            renderer,
            "Executables shown in the Launcher menu",
            l.header.loc.x,
            l.header,
        ));
        for (i, row) in l.rows.iter().enumerate() {
            let tx = row.path.loc.x + FIELD_TEXT_PAD;
            let shown = text.fit(&self.entries[i], row.path.size.w - 2 * FIELD_TEXT_PAD);
            elems.extend(ui::text_in(text, renderer, &shown, tx, row.path));
            elems.extend(ui::text_centered(text, renderer, "Change…", row.change));
            elems.extend(ui::text_centered(text, renderer, "Remove", row.remove));
        }
        elems.extend(ui::text_centered(text, renderer, "Add…", l.add));

        let button = |hit: PanelHit| {
            if hovered == Some(hit) {
                BUTTON_HOT
            } else {
                BUTTON
            }
        };
        for (i, row) in l.rows.iter().enumerate() {
            elems.push(ui::solid(&mut s[3 * i], row.path, FIELD_BG));
            elems.push(ui::solid(
                &mut s[3 * i + 1],
                row.change,
                button(PanelHit::Change(i)),
            ));
            elems.push(ui::solid(
                &mut s[3 * i + 2],
                row.remove,
                button(PanelHit::Remove(i)),
            ));
        }
        elems.push(ui::solid(&mut s[3 * n], l.add, button(PanelHit::Add)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_name_is_the_file_name() {
        assert_eq!(exe_name("/usr/bin/alacritty"), "alacritty");
        assert_eq!(exe_name("firefox"), "firefox");
        assert_eq!(exe_name("/"), "/");
    }
}
