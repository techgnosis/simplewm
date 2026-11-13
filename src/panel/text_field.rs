//! Editable one-line text fields (see `Field`): which one is being edited,
//! keyboard editing, mapping a click to a caret position, and drawing.

use smithay::{
    backend::renderer::{element::solid::SolidColorBuffer, gles::GlesRenderer},
    input::keyboard::keysyms,
    utils::{Point, Rectangle},
};

use super::{
    CARET, FIELD_BG, FIELD_BG_EDIT, FIELD_EDIT_RING, FIELD_TEXT_PAD, Field, PanelId, Rect,
};
use crate::{
    text::TextRenderer,
    ui::{self, CustomElement},
};

impl super::Panels {
    pub(super) fn field_text(&self, f: Field) -> &String {
        match f {
            Field::PickName => &self.picker.name,
            Field::NetAddress => &self.net.address,
            Field::NetGateway => &self.net.gateway,
            Field::SudoPassword => &self.sudo.password,
        }
    }

    fn field_text_mut(&mut self, f: Field) -> &mut String {
        match f {
            Field::PickName => &mut self.picker.name,
            Field::NetAddress => &mut self.net.address,
            Field::NetGateway => &mut self.net.gateway,
            Field::SudoPassword => &mut self.sudo.password,
        }
    }

    pub fn begin_edit(&mut self, field: Field, cursor: usize) {
        self.editing = Some(field);
        self.cursor = cursor.min(self.field_text(field).chars().count());
    }

    pub fn stop_editing(&mut self) {
        self.editing = None;
    }

    /// Handle a key press while a text field is being edited.
    pub fn key(&mut self, sym: u32, ch: Option<char>, ctrl: bool) {
        let Some(f) = self.editing else {
            return;
        };
        let len = self.field_text(f).chars().count();
        match sym {
            keysyms::KEY_BackSpace => {
                if self.cursor > 0 {
                    let c = self.cursor - 1;
                    remove_char(self.field_text_mut(f), c);
                    self.cursor -= 1;
                }
            }
            keysyms::KEY_Delete | keysyms::KEY_KP_Delete => {
                if self.cursor < len {
                    let c = self.cursor;
                    remove_char(self.field_text_mut(f), c);
                }
            }
            keysyms::KEY_Left | keysyms::KEY_KP_Left => self.cursor = self.cursor.saturating_sub(1),
            keysyms::KEY_Right | keysyms::KEY_KP_Right => self.cursor = (self.cursor + 1).min(len),
            keysyms::KEY_Home | keysyms::KEY_KP_Home => self.cursor = 0,
            keysyms::KEY_End | keysyms::KEY_KP_End => self.cursor = len,
            keysyms::KEY_Tab | keysyms::KEY_ISO_Left_Tab => {
                if let Some(next) = f.next() {
                    let end = self.field_text(next).chars().count();
                    self.begin_edit(next, end);
                }
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter if f.panel() == PanelId::Network => {
                self.network_apply()
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter if f == Field::SudoPassword => {
                self.sudo_submit()
            }
            keysyms::KEY_Escape if f == Field::SudoPassword => self.sudo_cancel(),
            keysyms::KEY_Return | keysyms::KEY_KP_Enter | keysyms::KEY_Escape => {
                self.stop_editing()
            }
            _ => {
                if let Some(c) = ch
                    && !ctrl
                    && !c.is_control()
                {
                    let cursor = self.cursor;
                    insert_char(self.field_text_mut(f), cursor, c);
                    self.cursor += 1;
                }
            }
        }
    }
}

/// Focus ring just outside a field being edited.
fn edit_ring(field: Rect) -> Rect {
    Rectangle::new(field.loc - Point::from((1, 1)), field.size + (2, 2).into())
}

/// What part of a field's text is visible.
struct FieldView {
    /// Index (in chars) of the first visible char.
    start: usize,
    visible: String,
    /// Caret x offset from the text start, when editing.
    caret_x: Option<i32>,
}

fn field_view(text: &TextRenderer, s: &str, cursor: Option<usize>, width: i32) -> FieldView {
    let chars: Vec<char> = s.chars().collect();
    let substr = |a: usize, b: usize| chars[a..b].iter().collect::<String>();

    if text.measure(s) <= width {
        return FieldView {
            start: 0,
            visible: s.to_string(),
            caret_x: cursor.map(|c| text.measure(&substr(0, c))),
        };
    }
    match cursor {
        // Scroll so the caret is visible.
        Some(c) => {
            let mut start = 0;
            while start < c && text.measure(&substr(start, c)) > width {
                start += 1;
            }
            let mut end = c;
            while end < chars.len() && text.measure(&substr(start, end + 1)) <= width {
                end += 1;
            }
            FieldView {
                start,
                visible: substr(start, end),
                caret_x: Some(text.measure(&substr(start, c))),
            }
        }
        None => FieldView {
            start: 0,
            visible: text.fit(s, width),
            caret_x: None,
        },
    }
}

/// Char index in field `rect` (showing `value`) nearest to pointer x `x`.
pub(super) fn field_char_at(
    text: &TextRenderer,
    value: &str,
    cursor: Option<usize>,
    rect: Rect,
    x: f64,
) -> usize {
    let view = field_view(text, value, cursor, rect.size.w - 2 * FIELD_TEXT_PAD);
    let mut pos = (rect.loc.x + FIELD_TEXT_PAD) as f64;
    let mut idx = view.start;
    for ch in value.chars().skip(view.start) {
        let adv = text.measure(&ch.to_string()) as f64;
        if x < pos + adv / 2.0 {
            break;
        }
        pos += adv;
        idx += 1;
    }
    idx
}

/// Draw an editable text field: its text (or a dimmed `placeholder` when
/// empty), the caret and focus ring when `cursor` is set. Uses 3 buffers.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_field(
    text: &mut TextRenderer,
    renderer: &mut GlesRenderer,
    s: &mut [SolidColorBuffer],
    rect: Rect,
    value: &str,
    placeholder: &str,
    cursor: Option<usize>,
    elems: &mut Vec<CustomElement>,
) {
    let tx = rect.loc.x + FIELD_TEXT_PAD;
    let view = field_view(text, value, cursor, rect.size.w - 2 * FIELD_TEXT_PAD);
    if value.is_empty() && !placeholder.is_empty() {
        elems.extend(ui::text_in_alpha(
            text,
            renderer,
            placeholder,
            tx,
            rect,
            0.35,
        ));
    } else {
        elems.extend(ui::text_in(text, renderer, &view.visible, tx, rect));
    }
    if let Some(cx) = view.caret_x {
        let caret = Rectangle::new(
            (tx + cx, rect.loc.y + 4).into(),
            (1, rect.size.h - 8).into(),
        );
        elems.push(ui::solid(&mut s[0], caret, CARET));
    }
    let editing = cursor.is_some();
    elems.push(ui::solid(
        &mut s[1],
        rect,
        if editing { FIELD_BG_EDIT } else { FIELD_BG },
    ));
    if editing {
        elems.push(ui::solid(&mut s[2], edit_ring(rect), FIELD_EDIT_RING));
    }
}

fn byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

fn insert_char(s: &mut String, char_idx: usize, c: char) {
    let b = byte_index(s, char_idx);
    s.insert(b, c);
}

fn remove_char(s: &mut String, char_idx: usize) {
    let b = byte_index(s, char_idx);
    if b < s.len() {
        s.remove(b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_by_char_not_byte() {
        let mut s = "héllo".to_string();
        insert_char(&mut s, 2, 'X');
        assert_eq!(s, "héXllo");
        remove_char(&mut s, 1);
        assert_eq!(s, "hXllo");
        insert_char(&mut s, 99, '!');
        assert_eq!(s, "hXllo!");
        remove_char(&mut s, 99);
        assert_eq!(s, "hXllo!");
    }

    #[test]
    fn field_view_scrolls_to_the_caret() {
        let text = TextRenderer::new();
        let s = "a fairly long value that will not fit in a narrow field";
        let width = text.measure("a fairly long");
        // Short text: all of it, caret where asked.
        let v = field_view(&text, "abc", Some(1), width);
        assert_eq!((v.start, v.visible.as_str()), (0, "abc"));
        assert_eq!(v.caret_x, Some(text.measure("a")));
        // Not editing: the start, cut to fit.
        let v = field_view(&text, s, None, width);
        assert_eq!(v.start, 0);
        assert!(text.measure(&v.visible) <= width);
        // Caret at the end: scrolled so the end shows, caret inside the field.
        let end = s.chars().count();
        let v = field_view(&text, s, Some(end), width);
        assert!(v.start > 0);
        assert!(s.ends_with(&v.visible));
        assert!(v.caret_x.unwrap() <= width);
    }

    #[test]
    fn click_maps_to_nearest_char() {
        let text = TextRenderer::new();
        let rect = Rectangle::new((100, 0).into(), (300, 26).into());
        let x0 = (100 + FIELD_TEXT_PAD) as f64;
        assert_eq!(field_char_at(&text, "abc", None, rect, 0.0), 0);
        assert_eq!(
            field_char_at(
                &text,
                "abc",
                None,
                rect,
                x0 + text.measure("ab") as f64 + 0.5
            ),
            2
        );
        assert_eq!(field_char_at(&text, "abc", None, rect, 1000.0), 3);
    }
}
