//! Tiny text renderer: rasterizes strings with fontdue into ARGB buffers.

use std::collections::HashMap;

use fontdue::{Font, FontSettings};
use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::Transform,
};

static FONT_DATA: &[u8] = include_bytes!("../assets/DejaVuSans.ttf");

pub const FONT_PX: f32 = 14.0;

pub struct TextRenderer {
    font: Font,
    cache: HashMap<String, (MemoryRenderBuffer, (i32, i32))>,
}

impl TextRenderer {
    pub fn new() -> Self {
        let font = Font::from_bytes(FONT_DATA, FontSettings::default()).expect("bad bundled font");
        Self {
            font,
            cache: HashMap::new(),
        }
    }

    /// Height of a line of text in pixels.
    pub fn line_height(&self) -> i32 {
        let m = self.font.horizontal_line_metrics(FONT_PX).unwrap();
        (m.ascent - m.descent).ceil() as i32
    }

    /// Width of the text in pixels.
    pub fn measure(&self, text: &str) -> i32 {
        text.chars()
            .map(|c| self.font.metrics(c, FONT_PX).advance_width)
            .sum::<f32>()
            .ceil() as i32
    }

    /// Truncate `text` with an ellipsis so it fits in `max_width` pixels.
    pub fn fit(&self, text: &str, max_width: i32) -> String {
        if self.measure(text) <= max_width {
            return text.to_string();
        }
        let ellipsis = "…";
        let budget = max_width - self.measure(ellipsis);
        let mut out = String::new();
        let mut width = 0.0;
        for c in text.chars() {
            let adv = self.font.metrics(c, FONT_PX).advance_width;
            if (width + adv).ceil() as i32 > budget {
                break;
            }
            width += adv;
            out.push(c);
        }
        if out.is_empty() && budget <= 0 {
            return String::new();
        }
        out.push_str(ellipsis);
        out
    }

    /// Break `text` into lines no wider than `max_width` pixels: at the last space
    /// where there is one (dropping it), mid-word where a word alone is too wide.
    pub fn wrap(&self, text: &str, max_width: i32) -> Vec<String> {
        let adv = |c: char| self.font.metrics(c, FONT_PX).advance_width;
        let mut lines = Vec::new();
        let mut line = String::new();
        let mut width = 0.0f32;
        // Byte index just after the last space in `line`.
        let mut after_space: Option<usize> = None;
        for c in text.chars() {
            let a = adv(c);
            if !line.is_empty() && (width + a).ceil() as i32 > max_width {
                if c == ' ' {
                    lines.push(std::mem::take(&mut line).trim_end().to_string());
                    width = 0.0;
                    after_space = None;
                    continue;
                }
                let rest = match after_space.take() {
                    Some(i) => line.split_off(i),
                    None => String::new(),
                };
                lines.push(std::mem::replace(&mut line, rest).trim_end().to_string());
                width = line.chars().map(adv).sum();
            }
            line.push(c);
            width += a;
            if c == ' ' {
                after_space = Some(line.len());
            }
        }
        if !line.is_empty() || lines.is_empty() {
            lines.push(line);
        }
        lines
    }

    /// Returns a (cached) white text buffer and its size.
    pub fn get(&mut self, text: &str) -> Option<(MemoryRenderBuffer, (i32, i32))> {
        if text.is_empty() {
            return None;
        }
        if let Some(entry) = self.cache.get(text) {
            return Some(entry.clone());
        }
        if self.cache.len() > 512 {
            self.cache.clear();
        }
        let entry = self.rasterize(text);
        self.cache.insert(text.to_string(), entry.clone());
        Some(entry)
    }

    fn rasterize(&self, text: &str) -> (MemoryRenderBuffer, (i32, i32)) {
        let lm = self.font.horizontal_line_metrics(FONT_PX).unwrap();
        let width = self.measure(text).max(1);
        let height = self.line_height().max(1);
        let baseline = lm.ascent.round() as i32;

        // Premultiplied ARGB8888 (little endian: B, G, R, A).
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let mut pen_x = 0.0f32;
        for c in text.chars() {
            let (metrics, bitmap) = self.font.rasterize(c, FONT_PX);
            let gx = (pen_x + metrics.xmin as f32).round() as i32;
            let gy = baseline - metrics.height as i32 - metrics.ymin;
            for row in 0..metrics.height as i32 {
                for col in 0..metrics.width as i32 {
                    let x = gx + col;
                    let y = gy + row;
                    if x < 0 || y < 0 || x >= width || y >= height {
                        continue;
                    }
                    let a = bitmap[(row * metrics.width as i32 + col) as usize];
                    let idx = ((y * width + x) * 4) as usize;
                    let prev = pixels[idx + 3];
                    let v = a.max(prev);
                    pixels[idx..idx + 4].copy_from_slice(&[v, v, v, v]);
                }
            }
            pen_x += metrics.advance_width;
        }

        let buffer = MemoryRenderBuffer::from_slice(
            &pixels,
            Fourcc::Argb8888,
            (width, height),
            1,
            Transform::Normal,
            None,
        );
        (buffer, (width, height))
    }
}

/// The default arrow cursor, generated from ASCII art.
pub fn arrow_cursor() -> MemoryRenderBuffer {
    const ART: &[&str] = &[
        "X           ",
        "XX          ",
        "X.X         ",
        "X..X        ",
        "X...X       ",
        "X....X      ",
        "X.....X     ",
        "X......X    ",
        "X.......X   ",
        "X........X  ",
        "X.........X ",
        "X......XXXXX",
        "X...X..X    ",
        "X..XX..X    ",
        "X.X  X..X   ",
        "XX   X..X   ",
        "X     X..X  ",
        "      X..X  ",
        "       XX   ",
    ];
    let scale = 1;
    let w = ART[0].len() as i32 * scale;
    let h = ART.len() as i32 * scale;
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    for (y, row) in ART.iter().enumerate() {
        for (x, ch) in row.chars().enumerate() {
            let px = match ch {
                'X' => [0, 0, 0, 255],
                '.' => [255, 255, 255, 255],
                _ => continue,
            };
            for dy in 0..scale {
                for dx in 0..scale {
                    let xx = x as i32 * scale + dx;
                    let yy = y as i32 * scale + dy;
                    let idx = ((yy * w + xx) * 4) as usize;
                    pixels[idx..idx + 4].copy_from_slice(&px);
                }
            }
        }
    }
    MemoryRenderBuffer::from_slice(
        &pixels,
        Fourcc::Argb8888,
        (w, h),
        1,
        Transform::Normal,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_breaks_at_spaces_and_inside_long_words() {
        let t = TextRenderer::new();
        assert_eq!(t.wrap("", 100), vec![String::new()]);
        assert_eq!(t.wrap("short", 100), vec!["short"]);
        let w = t.measure("hello world");
        assert_eq!(t.wrap("hello world again", w), vec!["hello world", "again"]);
        let lines = t.wrap(&"x".repeat(50), t.measure("xxxxxxxxxx"));
        assert_eq!(lines.len(), 5);
        assert!(lines.iter().all(|l| l == "xxxxxxxxxx"));
        let text = "pw.node [W] some long message that goes on and on, /a/very/long/path/name";
        let lines = t.wrap(text, 120);
        assert!(lines.iter().all(|l| t.measure(l) <= 120), "{lines:?}");
        assert_eq!(lines.join(" ").replace(' ', ""), text.replace(' ', ""));
    }
}
