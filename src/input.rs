use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    desktop::Window,
    input::{
        keyboard::{FilterResult, keysyms},
        pointer::{AxisFrame, ButtonEvent, CursorImageStatus, Focus, GrabStartData, MotionEvent},
    },
    utils::{Logical, Point, SERIAL_COUNTER, Serial},
};

use crate::{
    grabs::{Edges, MoveGrab, PanelMoveGrab, ResizeGrab},
    panel::{APPLETS, PanelHit, PanelId},
    state::{Backend, State},
    ui::{self, BarButton, Frame, Hit, MenuKind, bar_buttons},
};

const BTN_LEFT: u32 = 0x110;

enum KeyAction {
    Vt(i32),
    Panel(u32, Option<char>, bool),
    Picker(u32, Option<char>, bool),
    CancelScreenshot,
    Swallow,
}

enum Press {
    Consume,
    Forward,
}

impl State {
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event } => {
                let serial = SERIAL_COUNTER.next_serial();
                let time = Event::time_msec(&event);
                let key_state = event.state();
                let keyboard = self.seat.get_keyboard().unwrap();
                // Intercepted keys: VT switches on a TTY, and typing into the
                // Control Panel while one of its text fields is being edited.
                let action = keyboard.input::<KeyAction, _>(
                    self,
                    event.key_code(),
                    key_state,
                    serial,
                    time,
                    |state, mods, keysym| {
                        let sym = keysym.modified_sym().raw();
                        let pressed = key_state == KeyState::Pressed;
                        if pressed && (keysyms::KEY_XF86Switch_VT_1..=keysyms::KEY_XF86Switch_VT_12).contains(&sym) {
                            return FilterResult::Intercept(KeyAction::Vt(
                                (sym - keysyms::KEY_XF86Switch_VT_1 + 1) as i32,
                            ));
                        }
                        if state.ui.shot.is_some() && sym == keysyms::KEY_Escape {
                            return FilterResult::Intercept(if pressed {
                                KeyAction::CancelScreenshot
                            } else {
                                KeyAction::Swallow
                            });
                        }
                        // The File Picker takes all keys (Enter, Escape, arrows, typing).
                        if state.panels.active == Some(PanelId::FilePicker) {
                            if pressed {
                                let ch = keysym.modified_sym().key_char();
                                return FilterResult::Intercept(KeyAction::Picker(sym, ch, mods.ctrl));
                            }
                            return FilterResult::Intercept(KeyAction::Swallow);
                        }
                        let editing_panel = state.panels.editing.map(|f| f.panel());
                        if editing_panel.is_some() && editing_panel == state.panels.active {
                            if pressed {
                                let ch = keysym.modified_sym().key_char();
                                return FilterResult::Intercept(KeyAction::Panel(sym, ch, mods.ctrl));
                            }
                            return FilterResult::Intercept(KeyAction::Swallow);
                        }
                        FilterResult::Forward
                    },
                );
                match action {
                    Some(KeyAction::Vt(vt)) => {
                        if let Some(Backend::Udev(udev)) = self.backend.as_mut() {
                            udev.change_vt(vt);
                        }
                    }
                    Some(KeyAction::Panel(sym, ch, ctrl)) => self.panels.key(sym, ch, ctrl),
                    Some(KeyAction::Picker(sym, ch, ctrl)) => self.picker_key(sym, ch, ctrl),
                    Some(KeyAction::CancelScreenshot) => self.cancel_screenshot(),
                    Some(KeyAction::Swallow) | None => {}
                }
            }
            InputEvent::PointerMotion { event } => {
                let serial = SERIAL_COUNTER.next_serial();
                let pos = self.pointer_location + event.delta();
                self.on_motion(pos, serial, event.time_msec());
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let serial = SERIAL_COUNTER.next_serial();
                let out = self.output_geometry();
                let pos = event.position_transformed(out.size) + out.loc.to_f64();
                self.on_motion(pos, serial, event.time_msec());
            }
            InputEvent::PointerButton { event } => {
                let serial = SERIAL_COUNTER.next_serial();
                let button = event.button_code();
                let state = event.state();
                let pointer = self.seat.get_pointer().unwrap();

                // Selecting a screenshot region takes over the pointer.
                if self.ui.shot.is_some() {
                    self.screenshot_button(button, state == ButtonState::Pressed);
                    return;
                }

                if state == ButtonState::Pressed && !pointer.is_grabbed() {
                    if let Press::Consume = self.handle_press(button, serial) {
                        return;
                    }
                }
                pointer.button(
                    self,
                    &ButtonEvent {
                        button,
                        state,
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerAxis { event } => {
                // Scrolling over the Environment, Desktop Portal or File Picker window
                // scrolls its list.
                if let Hit::Panel(id @ (PanelId::Environment | PanelId::Portal | PanelId::FilePicker), _) =
                    self.hit_test(self.pointer_location)
                {
                    let amount = event
                        .amount(Axis::Vertical)
                        .unwrap_or_else(|| event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.0);
                    if id == PanelId::FilePicker {
                        self.panels.picker.scroll_by(amount);
                    } else {
                        self.panels.scroll_rows(id, amount);
                    }
                    return;
                }
                let source = event.source();
                let h_amount = event
                    .amount(Axis::Horizontal)
                    .unwrap_or_else(|| event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.0);
                let v_amount = event
                    .amount(Axis::Vertical)
                    .unwrap_or_else(|| event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.0);
                let mut frame = AxisFrame::new(event.time_msec()).source(source);
                if h_amount != 0.0 {
                    frame = frame.value(Axis::Horizontal, h_amount);
                    if let Some(d) = event.amount_v120(Axis::Horizontal) {
                        frame = frame.v120(Axis::Horizontal, d as i32);
                    }
                }
                if v_amount != 0.0 {
                    frame = frame.value(Axis::Vertical, v_amount);
                    if let Some(d) = event.amount_v120(Axis::Vertical) {
                        frame = frame.v120(Axis::Vertical, d as i32);
                    }
                }
                if source == AxisSource::Finger {
                    if event.amount(Axis::Horizontal) == Some(0.0) {
                        frame = frame.stop(Axis::Horizontal);
                    }
                    if event.amount(Axis::Vertical) == Some(0.0) {
                        frame = frame.stop(Axis::Vertical);
                    }
                }
                let pointer = self.seat.get_pointer().unwrap();
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            _ => {}
        }
    }

    fn on_motion(&mut self, pos: Point<f64, Logical>, serial: Serial, time: u32) {
        let out = self.output_geometry().to_f64();
        let pos: Point<f64, Logical> = (
            pos.x.clamp(out.loc.x, out.loc.x + out.size.w - 1.0),
            pos.y.clamp(out.loc.y, out.loc.y + out.size.h - 1.0),
        )
            .into();
        self.pointer_location = pos;

        let pointer = self.seat.get_pointer().unwrap();
        let focus = match self.hit_test(pos) {
            Hit::Content(_, surface, loc) if self.ui.shot.is_none() => Some((surface, loc)),
            _ => None,
        };
        if focus.is_none() && !pointer.is_grabbed() {
            self.cursor_status = CursorImageStatus::default_named();
        }
        pointer.motion(self, focus, &MotionEvent { location: pos, serial, time });
        pointer.frame(self);
    }

    fn handle_press(&mut self, button: u32, serial: Serial) -> Press {
        let pos = self.pointer_location;
        let hit = self.hit_test(pos);

        // Any click while a menu is open closes it.
        if let Some(menu) = self.ui.menu.take() {
            match hit {
                Hit::Menu(Some(i)) if button == BTN_LEFT => self.menu_action(menu.kind, i),
                Hit::BarButton(b) if button == BTN_LEFT => {
                    // Clicking the button of the open menu just closes it.
                    let same = b == BarButton::Launcher && matches!(menu.kind, MenuKind::Launcher(_));
                    if !same {
                        self.bar_button(b);
                    }
                }
                _ => {}
            }
            return Press::Consume;
        }

        if button != BTN_LEFT {
            return match hit {
                Hit::Content(w, ..) => {
                    self.focus_window(Some(&w));
                    Press::Forward
                }
                _ => Press::Consume,
            };
        }

        match hit {
            Hit::BarButton(b) => {
                self.bar_button(b);
                Press::Consume
            }
            Hit::Panel(id, h) => {
                self.focus_panel(id);
                match h {
                    PanelHit::Title => {
                        self.panels.stop_editing();
                        self.start_panel_move(id, button, serial);
                        return Press::Forward;
                    }
                    // Closing the File Picker cancels the app's request.
                    PanelHit::Close if id == PanelId::FilePicker => {
                        self.finish_pick(crate::portal::PickResult::Cancelled);
                    }
                    PanelHit::Close => {
                        self.panels.close(id);
                        self.focus_top();
                    }
                    PanelHit::Applet(i) => {
                        if let Some((_, applet)) = APPLETS.get(i) {
                            if *applet == PanelId::Environment {
                                let rows = self.environment_rows();
                                self.panels.set_environment(rows);
                            }
                            if *applet == PanelId::Portal {
                                self.refresh_portal_check();
                            }
                            let out = self.output_geometry();
                            self.panels.show(*applet, out);
                            self.focus_panel(*applet);
                        }
                    }
                    PanelHit::Quit => {
                        tracing::info!("quit requested");
                        self.panels.stop_editing();
                        self.loop_signal.stop();
                    }
                    PanelHit::Field(i, idx) => self.panels.begin_edit(i, idx),
                    PanelHit::Remove(i) => self.panels.remove_entry(i),
                    PanelHit::Add => self.panels.add_entry(),
                    PanelHit::Refresh => self.refresh_portal_check(),
                    PanelHit::Pick(h) => self.picker_click(h),
                    PanelHit::Body => self.panels.stop_editing(),
                }
                Press::Consume
            }
            Hit::Close(w) => {
                if let Some(t) = w.toplevel() {
                    t.send_close();
                }
                Press::Consume
            }
            Hit::SizeButton(w) => {
                self.focus_window(Some(&w));
                if let Some(content) = self.space.element_geometry(&w) {
                    let frame = Frame::new(content, &self.ui.text);
                    let anchor = (frame.size.loc.x, frame.size.loc.y + frame.size.size.h + 2).into();
                    let out = self.output_geometry();
                    self.ui.open_menu(MenuKind::Size(w), anchor, out);
                }
                Press::Consume
            }
            Hit::Title(w) => {
                self.focus_window(Some(&w));
                self.start_move(w, button, serial);
                Press::Forward
            }
            Hit::Resize(w, edges) => {
                self.focus_window(Some(&w));
                self.start_resize(w, edges, button, serial);
                Press::Forward
            }
            Hit::Content(w, ..) => {
                self.focus_window(Some(&w));
                Press::Forward
            }
            Hit::Inert(w) => {
                self.focus_window(Some(&w));
                Press::Consume
            }
            Hit::Menu(_) | Hit::Bar | Hit::Background => Press::Consume,
        }
    }

    fn start_move(&mut self, window: Window, button: u32, serial: Serial) {
        let Some(initial_loc) = self.space.element_location(&window) else {
            return;
        };
        let pointer = self.seat.get_pointer().unwrap();
        let start_data = GrabStartData {
            focus: None,
            button,
            location: self.pointer_location,
        };
        let grab = MoveGrab {
            start_data,
            window,
            initial_loc,
        };
        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    fn start_resize(&mut self, window: Window, edges: Edges, button: u32, serial: Serial) {
        let pointer = self.seat.get_pointer().unwrap();
        let start_data = GrabStartData {
            focus: None,
            button,
            location: self.pointer_location,
        };
        let grab = ResizeGrab::start(self, start_data, window, edges);
        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    fn start_panel_move(&mut self, id: PanelId, button: u32, serial: Serial) {
        let Some(initial_loc) = self.panels.loc(id) else {
            return;
        };
        let pointer = self.seat.get_pointer().unwrap();
        let start_data = GrabStartData {
            focus: None,
            button,
            location: self.pointer_location,
        };
        pointer.set_grab(self, PanelMoveGrab { start_data, id, initial_loc }, serial, Focus::Clear);
    }

    fn bar_button(&mut self, b: BarButton) {
        let out = self.output_geometry();
        match b {
            BarButton::ControlPanel => {
                self.panels.show(PanelId::ControlPanel, out);
                self.focus_panel(PanelId::ControlPanel);
            }
            BarButton::Screenshot => self.start_screenshot(),
            BarButton::Launcher => {
                let Some((_, rect)) = bar_buttons(&self.ui.text, out).into_iter().find(|(x, _)| *x == b) else {
                    return;
                };
                let anchor = (rect.loc.x, out.loc.y + ui::BAR_HEIGHT).into();
                let paths = self.panels.launcher_paths();
                self.ui.open_menu(MenuKind::Launcher(paths), anchor, out);
            }
        }
    }

    fn menu_action(&mut self, kind: MenuKind, index: usize) {
        match kind {
            MenuKind::Launcher(paths) => {
                if let Some(path) = paths.get(index) {
                    self.spawn(path);
                }
            }
            MenuKind::Size(window) => self.apply_size(&window, index),
        }
    }

    /// Move/resize a window to one of the regions in `ui::SIZE_OPTIONS`.
    fn apply_size(&mut self, window: &Window, index: usize) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        let Some((_, [x0, y0, x1, y1])) = ui::SIZE_OPTIONS.get(index).copied() else {
            return;
        };

        // Usable area below the top bar, split at the middle in each direction.
        let out = self.output_geometry();
        let area_x = [out.loc.x, out.loc.x + out.size.w / 2, out.loc.x + out.size.w];
        let area_top = out.loc.y + ui::BAR_HEIGHT;
        let area_h = out.size.h - ui::BAR_HEIGHT;
        let area_y = [area_top, area_top + area_h / 2, area_top + area_h];

        // Region covers the whole frame; shrink it to the client content.
        let b = ui::BORDER;
        let x = area_x[x0 as usize] + b;
        let y = area_y[y0 as usize] + ui::TITLE_HEIGHT;
        let w = area_x[x1 as usize] - b - x;
        let h = area_y[y1 as usize] - b - y;

        toplevel.with_pending_state(|state| {
            state.size = Some((w.max(1), h.max(1)).into());
        });
        toplevel.send_pending_configure();
        // Focus (and raise) first so the panel's stacking bookkeeping sees the
        // window's old position.
        self.focus_window(Some(window));
        self.space.map_element(window.clone(), (x, y), false);
    }
}
