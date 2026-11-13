use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    desktop::Window,
    input::{
        keyboard::{FilterResult, keysyms},
        pointer::{
            AxisFrame, ButtonEvent, CursorImageStatus, Focus, GrabStartData, MotionEvent,
            PointerHandle, RelativeMotionEvent,
        },
    },
    reexports::calloop::timer::{TimeoutAction, Timer},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER, Serial},
    wayland::pointer_constraints::{PointerConstraint, with_pointer_constraint},
};
use std::time::Duration;

use crate::{
    grabs::{Edges, MoveGrab, PanelMoveGrab, ResizeGrab},
    panel::{APPLETS, PanelHit, PanelId},
    state::{Backend, State},
    ui::{self, BarButton, Frame, Hit, MenuKind, bar_buttons},
};

const BTN_LEFT: u32 = 0x110;
/// Key repeat for simplewm's own windows, matching the rate given to clients.
const REPEAT_DELAY_MS: u64 = 200;
const REPEAT_RATE: u64 = 25;

/// Whether holding `sym` repeats it. Keys that act once (accept, cancel) don't.
fn repeats(sym: u32) -> bool {
    !matches!(
        sym,
        keysyms::KEY_Return
            | keysyms::KEY_KP_Enter
            | keysyms::KEY_Escape
            | keysyms::KEY_Tab
            | keysyms::KEY_ISO_Left_Tab
    )
}

#[derive(Clone, Copy)]
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
    fn run_key_action(&mut self, action: Option<KeyAction>) {
        match action {
            Some(KeyAction::Vt(vt)) => {
                if let Some(Backend::Udev(udev)) = self.backend.as_mut() {
                    udev.change_vt(vt);
                }
            }
            Some(KeyAction::Panel(sym, ch, ctrl)) => {
                self.panels.key(sym, ch, ctrl);
                self.after_panel_action();
            }
            Some(KeyAction::Picker(sym, ch, ctrl)) => self.picker_key(sym, ch, ctrl),
            Some(KeyAction::CancelScreenshot) => self.cancel_screenshot(),
            Some(KeyAction::Swallow) | None => {}
        }
    }

    /// After a click or key in a panel: open a window it asked for (the
    /// Password window), or move focus on if it closed itself.
    fn after_panel_action(&mut self) {
        if let Some(id) = self.panels.take_open_request() {
            let out = self.output_geometry();
            self.panels.show(id, out);
            self.focus_panel(id);
        } else if self.panels.active.is_none() {
            self.focus_top();
        }
    }

    fn start_key_repeat(&mut self, code: u32, action: KeyAction) {
        let timer = Timer::from_duration(Duration::from_millis(REPEAT_DELAY_MS));
        let token = self.loop_handle.insert_source(timer, move |_, _, state| {
            // Stop once the window the key went to no longer takes keys.
            let alive = match action {
                KeyAction::Picker(..) => state.panels.active == Some(PanelId::FilePicker),
                KeyAction::Panel(..) => state.panels.editing.is_some(),
                _ => false,
            };
            if !alive {
                state.key_repeat = None;
                return TimeoutAction::Drop;
            }
            state.run_key_action(Some(action));
            TimeoutAction::ToDuration(Duration::from_millis(1000 / REPEAT_RATE))
        });
        self.key_repeat = token.ok().map(|t| (code, t));
    }

    fn stop_key_repeat(&mut self) {
        if let Some((_, token)) = self.key_repeat.take() {
            self.loop_handle.remove(token);
        }
    }

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
                        if pressed
                            && (keysyms::KEY_XF86Switch_VT_1..=keysyms::KEY_XF86Switch_VT_12)
                                .contains(&sym)
                        {
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
                                return FilterResult::Intercept(KeyAction::Picker(
                                    sym, ch, mods.ctrl,
                                ));
                            }
                            return FilterResult::Intercept(KeyAction::Swallow);
                        }
                        let editing_panel = state.panels.editing.map(|f| f.panel());
                        if editing_panel.is_some() && editing_panel == state.panels.active {
                            if pressed {
                                let ch = keysym.modified_sym().key_char();
                                return FilterResult::Intercept(KeyAction::Panel(
                                    sym, ch, mods.ctrl,
                                ));
                            }
                            return FilterResult::Intercept(KeyAction::Swallow);
                        }
                        FilterResult::Forward
                    },
                );
                // simplewm's own windows get key repeat here; clients do their own.
                let code = event.key_code().raw();
                if key_state == KeyState::Pressed || self.key_repeat.is_some_and(|(c, _)| c == code)
                {
                    self.stop_key_repeat();
                }
                if key_state == KeyState::Pressed
                    && let Some(a @ (KeyAction::Panel(sym, ..) | KeyAction::Picker(sym, ..))) =
                        action
                    && repeats(sym)
                {
                    self.start_key_repeat(code, a);
                }
                self.run_key_action(action);
            }
            InputEvent::PointerMotion { event } => {
                let serial = SERIAL_COUNTER.next_serial();
                let pointer = self.seat.get_pointer().unwrap();
                let under = self
                    .content_under(self.pointer_location)
                    .map(|(_, s, loc)| (s, loc));
                // Relative motion reaches the surface under the pointer even when
                // a pointer lock keeps the cursor still (games, gamescope).
                pointer.relative_motion(
                    self,
                    under.clone(),
                    &RelativeMotionEvent {
                        delta: event.delta(),
                        delta_unaccel: event.delta_unaccel(),
                        utime: event.time(),
                    },
                );
                let pos = self.pointer_location + event.delta();
                if !pointer.is_grabbed()
                    && let Some((surface, _)) = &under
                    && !self.constraint_allows(surface, &pointer, pos)
                {
                    pointer.frame(self);
                    return;
                }
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

                if state == ButtonState::Pressed
                    && !pointer.is_grabbed()
                    && let Press::Consume = self.handle_press(button, serial)
                {
                    return;
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
                // Scrolling over the Environment, Desktop Portal, Network or File Picker window
                // scrolls its list.
                if let Hit::Panel(
                    id @ (PanelId::Environment
                    | PanelId::Portal
                    | PanelId::Network
                    | PanelId::FilePicker),
                    _,
                ) = self.hit_test(self.pointer_location)
                {
                    let amount = event.amount(Axis::Vertical).unwrap_or_else(|| {
                        event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.0
                    });
                    if id == PanelId::FilePicker {
                        // A wheel notch (15) moves three rows; touchpads scroll by pixels.
                        let scale = if event.source() == AxisSource::Wheel {
                            3.0 * 22.0 / 15.0
                        } else {
                            1.0
                        };
                        self.panels.picker.scroll_by(amount * scale);
                    } else {
                        self.panels.scroll_rows(id, amount);
                    }
                    return;
                }
                let source = event.source();
                let h_amount = event.amount(Axis::Horizontal).unwrap_or_else(|| {
                    event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.0
                });
                let v_amount = event.amount(Axis::Vertical).unwrap_or_else(|| {
                    event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.0
                });
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
        let entered = focus.as_ref().map(|(s, _)| s.clone());
        pointer.motion(
            self,
            focus,
            &MotionEvent {
                location: pos,
                serial,
                time,
            },
        );
        pointer.frame(self);
        if let Some(surface) = entered {
            self.maybe_activate_constraint(&surface, &pointer);
        }
    }

    /// The client surface under `pos`, with its window and global origin.
    fn content_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(Window, WlSurface, Point<f64, Logical>)> {
        match self.hit_test(pos) {
            Hit::Content(window, surface, loc) if self.ui.shot.is_none() => {
                Some((window, surface, loc))
            }
            _ => None,
        }
    }

    /// Global origin of `surface` if it is the one under the pointer.
    pub fn surface_origin_under_pointer(&self, surface: &WlSurface) -> Option<Point<f64, Logical>> {
        self.content_under(self.pointer_location)
            .filter(|(_, s, _)| s == surface)
            .map(|(_, _, loc)| loc)
    }

    /// Activate a pointer lock or confinement on `surface` if the pointer is over
    /// it, inside the constraint's region, and its window has keyboard focus (so
    /// merely passing over a game doesn't trap the pointer).
    pub fn maybe_activate_constraint(&self, surface: &WlSurface, pointer: &PointerHandle<State>) {
        let Some((window, _, origin)) = self
            .content_under(self.pointer_location)
            .filter(|(_, s, _)| s == surface)
        else {
            return;
        };
        if self.focused_window().as_ref() != Some(&window) {
            return;
        }
        let local = (self.pointer_location - origin).to_i32_round();
        with_pointer_constraint(surface, pointer, |c| {
            if let Some(c) = c
                && !c.is_active()
                && c.region().is_none_or(|r| r.contains(local))
            {
                c.activate();
            }
        });
    }

    /// Whether an active constraint on `surface` lets the pointer move to `pos`:
    /// a lock never does, a confinement only within its region of the surface.
    fn constraint_allows(
        &self,
        surface: &WlSurface,
        pointer: &PointerHandle<State>,
        pos: Point<f64, Logical>,
    ) -> bool {
        let confined = with_pointer_constraint(surface, pointer, |c| match c {
            Some(c) if c.is_active() => match &*c {
                PointerConstraint::Locked(_) => Some(None),
                PointerConstraint::Confined(confined) => Some(Some(confined.region().cloned())),
            },
            _ => None,
        });
        match confined {
            None => true,
            Some(None) => false,
            Some(Some(region)) => match self.content_under(pos) {
                Some((_, s, origin)) if &s == surface => {
                    region.is_none_or(|r| r.contains((pos - origin).to_i32_round()))
                }
                _ => false,
            },
        }
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
                    let same =
                        b == BarButton::Launcher && matches!(menu.kind, MenuKind::Launcher(_));
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
                    // Closing the Password window cancels what it was asked for.
                    PanelHit::Close if id == PanelId::Password => self.panels.sudo_cancel(),
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
                            if *applet == PanelId::Network {
                                self.panels.refresh_network();
                            }
                            if *applet == PanelId::Sound {
                                self.panels.refresh_sound();
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
                    PanelHit::Change(i) => self.choose_launcher_entry(Some(i)),
                    PanelHit::Remove(i) => self.panels.remove_entry(i),
                    PanelHit::Add => self.choose_launcher_entry(None),
                    PanelHit::Refresh => self.refresh_portal_check(),
                    PanelHit::NetApply => self.panels.network_apply(),
                    PanelHit::NetRefresh => self.panels.refresh_network(),
                    PanelHit::SoundCard(i) => self.panels.sound_choose(i),
                    PanelHit::SoundBar(percent) => {
                        self.panels.sound_set_volume(percent as f64 / 100.0)
                    }
                    PanelHit::SoundDown => self.panels.sound_step(-1.0),
                    PanelHit::SoundUp => self.panels.sound_step(1.0),
                    PanelHit::SudoOk => self.panels.sudo_submit(),
                    PanelHit::SudoCancel => self.panels.sudo_cancel(),
                    PanelHit::ChooseShotDir => {
                        self.panels.stop_editing();
                        self.choose_screenshot_dir();
                    }
                    PanelHit::Pick(h) => self.picker_click(h),
                    PanelHit::Body => self.panels.stop_editing(),
                }
                self.after_panel_action();
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
                    let anchor =
                        (frame.size.loc.x, frame.size.loc.y + frame.size.size.h + 2).into();
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
        pointer.set_grab(
            self,
            PanelMoveGrab {
                start_data,
                id,
                initial_loc,
            },
            serial,
            Focus::Clear,
        );
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
                let Some((_, rect)) = bar_buttons(&self.ui.text, out)
                    .into_iter()
                    .find(|(x, _)| *x == b)
                else {
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
        let area_x = [
            out.loc.x,
            out.loc.x + out.size.w / 2,
            out.loc.x + out.size.w,
        ];
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
