//! Screenshots: the Screenshot bar button enters selection mode, the user
//! drags out a rectangle, and on release that part of the screen is saved as a
//! PNG in the folder set in the Screenshot applet. Esc or a right click
//! cancels.
//!
//! The capture re-renders the scene (without the cursor, the selection
//! overlay and hover highlights) into an offscreen texture and reads the
//! selected region back. Encoding and writing the file happen on a worker
//! thread; the result comes back over a calloop channel and is shown as a
//! toast.

use std::path::{Path, PathBuf};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, ExportMem, Offscreen,
            damage::OutputDamageTracker,
            gles::{GlesRenderer, GlesTexture},
        },
    },
    reexports::calloop::channel::Sender,
    utils::{Logical, Physical, Point, Rectangle, Size, Transform},
};

use crate::{
    panel::PanelId,
    state::{Backend, State},
    ui::CLEAR_COLOR,
};

/// Selection mode, active from the bar click until release or cancel.
pub struct Selecting {
    /// Where the drag started; `None` until the button is pressed.
    pub anchor: Option<Point<f64, Logical>>,
}

impl Selecting {
    /// Selected rectangle (in whole pixels, including the pixels under both
    /// the anchor and the pointer).
    pub fn rect(&self, ptr: Point<f64, Logical>) -> Option<Rectangle<i32, Logical>> {
        let a = self.anchor?;
        let (x0, x1) = (
            a.x.min(ptr.x).floor() as i32,
            a.x.max(ptr.x).floor() as i32 + 1,
        );
        let (y0, y1) = (
            a.y.min(ptr.y).floor() as i32,
            a.y.max(ptr.y).floor() as i32 + 1,
        );
        Some(Rectangle::new((x0, y0).into(), (x1 - x0, y1 - y0).into()))
    }
}

/// Result of saving a screenshot, sent back from the worker thread.
pub type SaveResult = Result<PathBuf, String>;

/// RGB pixels, top row first.
struct Image {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

impl State {
    /// Bar button: enter selection mode, or ask for a folder first.
    pub fn start_screenshot(&mut self) {
        if self.panels.screenshot_dir().is_none() {
            let out = self.output_geometry();
            self.panels.show(PanelId::Screenshot, out);
            self.focus_panel(PanelId::Screenshot);
            self.ui.show_toast("Choose a folder for screenshots first");
            return;
        }
        self.ui.shot = Some(Selecting { anchor: None });
    }

    pub fn cancel_screenshot(&mut self) {
        self.ui.shot = None;
    }

    /// Pointer buttons while selecting: left press starts the drag, left
    /// release takes the shot, any other button cancels.
    pub fn screenshot_button(&mut self, button: u32, pressed: bool) {
        const BTN_LEFT: u32 = 0x110;
        let Some(sel) = self.ui.shot.as_mut() else {
            return;
        };
        match (button, pressed) {
            (BTN_LEFT, true) => sel.anchor = Some(self.pointer_location),
            (BTN_LEFT, false) => {
                // The release of the bar click that started selection mode
                // arrives here too; there's no anchor yet then.
                if sel.anchor.is_none() {
                    return;
                }
                let rect = sel.rect(self.pointer_location);
                self.ui.shot = None;
                match rect {
                    // A plain click (no drag) cancels.
                    Some(r) if r.size.w >= 2 && r.size.h >= 2 => self.take_screenshot(r),
                    _ => {}
                }
            }
            (_, true) => self.cancel_screenshot(),
            (_, false) => {}
        }
    }

    fn take_screenshot(&mut self, rect: Rectangle<i32, Logical>) {
        let Some(dir) = self.panels.screenshot_dir() else {
            return;
        };
        let image = match self.capture(rect) {
            Ok(image) => image,
            Err(err) => {
                tracing::error!(%err, "screenshot capture failed");
                self.ui.show_toast(&format!("Screenshot failed: {err}"));
                return;
            }
        };
        let tx: Sender<SaveResult> = self.screenshot_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(save_png(&dir, &image));
        });
    }

    /// Called on the event loop when the worker thread is done.
    pub fn screenshot_saved(&mut self, result: SaveResult) {
        match result {
            Ok(path) => {
                tracing::info!(?path, "saved screenshot");
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.ui.show_toast(&format!("Saved {name}"));
            }
            Err(err) => {
                tracing::error!(%err, "failed to save screenshot");
                self.ui.show_toast(&format!("Screenshot failed: {err}"));
            }
        }
    }

    fn capture(&mut self, rect: Rectangle<i32, Logical>) -> Result<Image, String> {
        let mut backend = self.backend.take().ok_or("no renderer")?;
        let renderer = match &mut backend {
            Backend::Winit(w) => w.backend.renderer(),
            Backend::Udev(u) => &mut u.renderer,
        };
        let result = self.capture_with(renderer, rect);
        self.backend = Some(backend);
        result
    }

    fn capture_with(
        &mut self,
        renderer: &mut GlesRenderer,
        rect: Rectangle<i32, Logical>,
    ) -> Result<Image, String> {
        let out = self.output_geometry();
        let rect = rect.intersection(out).ok_or("selection is off screen")?;
        // Output-relative; logical == physical (scale 1) everywhere in simplewm.
        let (x, y, w, h) = (
            rect.loc.x - out.loc.x,
            rect.loc.y - out.loc.y,
            rect.size.w,
            rect.size.h,
        );
        let size: Size<i32, Physical> = (out.size.w, out.size.h).into();

        let elements = self.build_elements(renderer, true);
        let err = |e: &dyn std::fmt::Debug| format!("{e:?}");
        let mut texture: GlesTexture = renderer
            .create_buffer(Fourcc::Abgr8888, (size.w, size.h).into())
            .map_err(|e| err(&e))?;
        let mapping = {
            let mut fb = renderer.bind(&mut texture).map_err(|e| err(&e))?;
            let mut damage = OutputDamageTracker::new(size, 1.0, Transform::Normal);
            damage
                .render_output(renderer, &mut fb, 0, &elements, CLEAR_COLOR)
                .map_err(|e| err(&e))?;
            // Offscreen textures are stored top row first (smithay's
            // projection already accounts for GL's bottom-up rows).
            renderer
                .copy_framebuffer(
                    &fb,
                    Rectangle::new((x, y).into(), (w, h).into()),
                    Fourcc::Abgr8888,
                )
                .map_err(|e| err(&e))?
        };
        let rgba = renderer.map_texture(&mapping).map_err(|e| err(&e))?;

        // RGBA -> RGB.
        let (w, h) = (w as usize, h as usize);
        let mut rgb = Vec::with_capacity(w * h * 3);
        for row in rgba.chunks_exact(w * 4).take(h) {
            for px in row.as_chunks::<4>().0 {
                rgb.extend_from_slice(&px[..3]);
            }
        }
        Ok(Image {
            width: w as u32,
            height: h as u32,
            rgb,
        })
    }
}

/// Write the image as simplewm-screenshot-<date>-<time>.png in `dir`,
/// creating the folder if needed.
fn save_png(dir: &Path, image: &Image) -> SaveResult {
    std::fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    let stamp = chrono::Local::now().format("%Y-%m-%d-%H%M%S").to_string();
    let mut path = dir.join(format!("simplewm-screenshot-{stamp}.png"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("simplewm-screenshot-{stamp}-{n}.png"));
        n += 1;
    }

    let file =
        std::fs::File::create(&path).map_err(|e| format!("can't write {}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), image.width, image.height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let result = encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(&image.rgb));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&path);
        return Err(format!("PNG encoding failed: {e}"));
    }
    Ok(path)
}
