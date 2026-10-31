//! Pointer grabs for moving and resizing windows.

use std::cell::RefCell;

use smithay::{
    desktop::Window,
    input::pointer::{
        AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
        GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
        GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
        RelativeMotionEvent,
    },
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Point, Rectangle, Size},
    wayland::{compositor::with_states, shell::xdg::SurfaceCachedState},
};

use crate::{state::State, ui};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Edges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

impl Edges {
    pub fn any(&self) -> bool {
        self.left || self.right || self.top || self.bottom
    }
}

pub fn edges_from_xdg(e: xdg_toplevel::ResizeEdge) -> Edges {
    use xdg_toplevel::ResizeEdge as R;
    Edges {
        left: matches!(e, R::Left | R::TopLeft | R::BottomLeft),
        right: matches!(e, R::Right | R::TopRight | R::BottomRight),
        top: matches!(e, R::Top | R::TopLeft | R::TopRight),
        bottom: matches!(e, R::Bottom | R::BottomLeft | R::BottomRight),
    }
}

// ---------------------------------------------------------------------------
// Forwarding boilerplate shared by both grabs
// ---------------------------------------------------------------------------

macro_rules! forward_rest {
    () => {
        fn relative_motion(
            &mut self,
            data: &mut State,
            handle: &mut PointerInnerHandle<'_, State>,
            focus: Option<(WlSurface, Point<f64, Logical>)>,
            event: &RelativeMotionEvent,
        ) {
            handle.relative_motion(data, focus, event);
        }

        fn button(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, event: &ButtonEvent) {
            handle.button(data, event);
            if !handle.current_pressed().contains(&self.start_data.button) {
                handle.unset_grab(self, data, event.serial, event.time, true);
            }
        }

        fn axis(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, details: AxisFrame) {
            handle.axis(data, details)
        }

        fn frame(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>) {
            handle.frame(data);
        }

        fn gesture_swipe_begin(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, event: &GestureSwipeBeginEvent) {
            handle.gesture_swipe_begin(data, event)
        }
        fn gesture_swipe_update(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, event: &GestureSwipeUpdateEvent) {
            handle.gesture_swipe_update(data, event)
        }
        fn gesture_swipe_end(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, event: &GestureSwipeEndEvent) {
            handle.gesture_swipe_end(data, event)
        }
        fn gesture_pinch_begin(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, event: &GesturePinchBeginEvent) {
            handle.gesture_pinch_begin(data, event)
        }
        fn gesture_pinch_update(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, event: &GesturePinchUpdateEvent) {
            handle.gesture_pinch_update(data, event)
        }
        fn gesture_pinch_end(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, event: &GesturePinchEndEvent) {
            handle.gesture_pinch_end(data, event)
        }
        fn gesture_hold_begin(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, event: &GestureHoldBeginEvent) {
            handle.gesture_hold_begin(data, event)
        }
        fn gesture_hold_end(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, event: &GestureHoldEndEvent) {
            handle.gesture_hold_end(data, event)
        }

        fn start_data(&self) -> &GrabStartData<State> {
            &self.start_data
        }
    };
}

// ---------------------------------------------------------------------------
// Move
// ---------------------------------------------------------------------------

pub struct MoveGrab {
    pub start_data: GrabStartData<State>,
    pub window: Window,
    pub initial_loc: Point<i32, Logical>,
}

impl PointerGrab<State> for MoveGrab {
    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        handle.motion(data, None, event);
        let delta = event.location - self.start_data.location;
        let mut loc = self.initial_loc + delta.to_i32_round();
        // Keep the title bar reachable below the top bar.
        let out = data.output_geometry();
        loc.y = loc.y.max(out.loc.y + ui::BAR_HEIGHT + ui::TITLE_HEIGHT);
        data.space.map_element(self.window.clone(), loc, false);
    }

    forward_rest!();

    fn unset(&mut self, _data: &mut State) {}
}

// ---------------------------------------------------------------------------
// Move the Control Panel
// ---------------------------------------------------------------------------

pub struct PanelMoveGrab {
    pub start_data: GrabStartData<State>,
    pub id: crate::panel::PanelId,
    pub initial_loc: Point<i32, Logical>,
}

impl PointerGrab<State> for PanelMoveGrab {
    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        handle.motion(data, None, event);
        let delta = event.location - self.start_data.location;
        let mut loc = self.initial_loc + delta.to_i32_round();
        let out = data.output_geometry();
        loc.y = loc.y.max(out.loc.y + ui::BAR_HEIGHT + ui::TITLE_HEIGHT);
        data.panels.set_loc(self.id, loc);
    }

    forward_rest!();

    fn unset(&mut self, _data: &mut State) {}
}

// ---------------------------------------------------------------------------
// Resize
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeState {
    Idle,
    Resizing { edges: Edges, initial: Rectangle<i32, Logical> },
    WaitingForLastCommit { edges: Edges, initial: Rectangle<i32, Logical> },
}

fn resize_state(window: &Window) -> &RefCell<ResizeState> {
    window.user_data().insert_if_missing(|| RefCell::new(ResizeState::Idle));
    window.user_data().get::<RefCell<ResizeState>>().unwrap()
}

pub struct ResizeGrab {
    start_data: GrabStartData<State>,
    window: Window,
    edges: Edges,
    initial: Rectangle<i32, Logical>,
}

impl ResizeGrab {
    pub fn start(state: &mut State, start_data: GrabStartData<State>, window: Window, edges: Edges) -> Self {
        let initial = state
            .space
            .element_geometry(&window)
            .unwrap_or_else(|| Rectangle::from_size(window.geometry().size));
        *resize_state(&window).borrow_mut() = ResizeState::Resizing { edges, initial };
        Self {
            start_data,
            window,
            edges,
            initial,
        }
    }
}

impl PointerGrab<State> for ResizeGrab {
    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        handle.motion(data, None, event);
        let delta = (event.location - self.start_data.location).to_i32_round::<i32>();

        let mut w = self.initial.size.w;
        let mut h = self.initial.size.h;
        if self.edges.left {
            w -= delta.x;
        }
        if self.edges.right {
            w += delta.x;
        }
        if self.edges.top {
            h -= delta.y;
        }
        if self.edges.bottom {
            h += delta.y;
        }

        let Some(toplevel) = self.window.toplevel() else {
            return;
        };
        let (min, max) = with_states(toplevel.wl_surface(), |states| {
            let mut guard = states.cached_state.get::<SurfaceCachedState>();
            let c = guard.current();
            (c.min_size, c.max_size)
        });
        let min_w = min.w.max(120);
        let min_h = min.h.max(60);
        let max_w = if max.w == 0 { i32::MAX } else { max.w };
        let max_h = if max.h == 0 { i32::MAX } else { max.h };
        let size: Size<i32, Logical> = (w.clamp(min_w, max_w.max(min_w)), h.clamp(min_h, max_h.max(min_h))).into();

        toplevel.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Resizing);
            state.size = Some(size);
        });
        toplevel.send_pending_configure();
    }

    forward_rest!();

    fn unset(&mut self, _data: &mut State) {
        if let Some(toplevel) = self.window.toplevel() {
            toplevel.with_pending_state(|state| {
                state.states.unset(xdg_toplevel::State::Resizing);
            });
            toplevel.send_pending_configure();
        }
        *resize_state(&self.window).borrow_mut() = ResizeState::WaitingForLastCommit {
            edges: self.edges,
            initial: self.initial,
        };
    }
}

/// When resizing from the left/top, keep the opposite edge fixed by moving the
/// window as its size changes.
pub fn handle_resize_commit(state: &mut State, surface: &WlSurface) {
    let Some(window) = state.window_for_surface(surface) else {
        return;
    };
    let current = *resize_state(&window).borrow();
    let (edges, initial, done) = match current {
        ResizeState::Idle => return,
        ResizeState::Resizing { edges, initial } => (edges, initial, false),
        ResizeState::WaitingForLastCommit { edges, initial } => (edges, initial, true),
    };

    if edges.left || edges.top {
        let geo = window.geometry();
        let Some(mut loc) = state.space.element_location(&window) else {
            return;
        };
        if edges.left {
            loc.x = initial.loc.x + initial.size.w - geo.size.w;
        }
        if edges.top {
            loc.y = initial.loc.y + initial.size.h - geo.size.h;
        }
        state.space.map_element(window.clone(), loc, false);
    }

    if done {
        *resize_state(&window).borrow_mut() = ResizeState::Idle;
    }
}
