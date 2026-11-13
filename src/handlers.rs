use std::cell::RefCell;

use smithay::{
    backend::{allocator::dmabuf::Dmabuf, renderer::ImportDma},
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_kde_decoration,
    delegate_output, delegate_pointer_constraints, delegate_presentation,
    delegate_relative_pointer, delegate_seat, delegate_shm, delegate_single_pixel_buffer,
    delegate_viewporter, delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{PopupKind, Window, find_popup_root_surface, get_popup_toplevel_coords},
    input::{
        Seat, SeatHandler, SeatState,
        pointer::{CursorImageStatus, Focus, GrabStartData, PointerHandle},
    },
    reexports::{
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
            shell::server::xdg_toplevel,
        },
        wayland_server::{
            Client, Resource,
            protocol::{wl_buffer, wl_seat, wl_surface::WlSurface},
        },
    },
    utils::{Logical, Point, Serial},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, get_parent,
            is_sync_subsurface, with_states,
        },
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        output::OutputHandler,
        pointer_constraints::{PointerConstraintsHandler, with_pointer_constraint},
        selection::{
            SelectionHandler,
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
                set_data_device_focus,
            },
        },
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            XdgToplevelSurfaceData, decoration::XdgDecorationHandler,
        },
        shm::{ShmHandler, ShmState},
    },
};

use smithay::{
    reexports::{
        wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::{
            Mode as KdeDecorationMode, OrgKdeKwinServerDecoration,
        },
        wayland_server::WEnum,
    },
    wayland::shell::kde::decoration::{KdeDecorationHandler, KdeDecorationState},
};

use crate::{
    grabs::{MoveGrab, ResizeGrab, ResizeState, edges_from_xdg},
    state::{ClientState, State},
};

// ---------------------------------------------------------------------------
// wl_compositor
// ---------------------------------------------------------------------------

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        smithay::backend::renderer::utils::on_commit_buffer_handler::<Self>(surface);

        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self.window_for_surface(&root) {
                window.on_commit();
            }
        }

        self.popups.commit(surface);
        handle_initial_configure(self, surface);
        crate::grabs::handle_resize_commit(self, surface);
    }
}

fn handle_initial_configure(state: &mut State, surface: &WlSurface) {
    if let Some(window) = state.window_for_surface(surface) {
        let toplevel = window.toplevel().unwrap();
        let initial_configure_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .unwrap()
                .lock()
                .unwrap()
                .initial_configure_sent
        });
        if !initial_configure_sent {
            toplevel.send_configure();
        }
        return;
    }

    if let Some(popup) = state.popups.find_popup(surface) {
        let PopupKind::Xdg(ref xdg) = popup else {
            return;
        };
        if !xdg.is_initial_configure_sent() {
            xdg.send_configure().expect("initial configure failed");
        }
    }
}

delegate_compositor!(State);

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}
delegate_shm!(State);

// ---------------------------------------------------------------------------
// dmabuf
// ---------------------------------------------------------------------------

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        let ok = self
            .renderer()
            .map(|r| r.import_dmabuf(&dmabuf, None).is_ok())
            .unwrap_or(false);
        if ok {
            let _ = notifier.successful::<State>();
        } else {
            notifier.failed();
        }
    }
}
delegate_dmabuf!(State);

// ---------------------------------------------------------------------------
// seat / data device
// ---------------------------------------------------------------------------

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<State> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client);
    }
}
delegate_seat!(State);

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}
impl ClientDndGrabHandler for State {}
impl ServerDndGrabHandler for State {}
delegate_data_device!(State);

impl OutputHandler for State {}
delegate_output!(State);

// ---------------------------------------------------------------------------
// xdg-shell
// ---------------------------------------------------------------------------

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Always use server side decorations; tell the client we're floating.
        surface.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        // The bar tab's label: a number and the owning process's name (its
        // app ID if the process can't be found).
        let pid = surface
            .wl_surface()
            .client()
            .and_then(|c| c.get_credentials(&self.display_handle).ok())
            .map(|c| c.pid);
        let name = pid
            .and_then(crate::ui::process_name)
            .or_else(|| {
                with_states(surface.wl_surface(), |states| {
                    states
                        .data_map
                        .get::<XdgToplevelSurfaceData>()
                        .and_then(|d| d.lock().unwrap().app_id.clone())
                })
            })
            .unwrap_or_else(|| "window".to_string());
        self.windows_tagged += 1;
        let tag = crate::ui::WindowTag {
            id: self.windows_tagged,
            name,
        };
        let window = Window::new_wayland_window(surface);
        window
            .user_data()
            .insert_if_missing(|| RefCell::new(ResizeState::Idle));
        window.user_data().insert_if_missing(|| tag);
        let loc = self.initial_window_location();
        self.space.map_element(window.clone(), loc, false);
        self.focus_window(Some(&window));
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let window = self
            .space
            .elements()
            .find(|w| w.toplevel() == Some(&surface))
            .cloned();
        if let Some(window) = window {
            self.ui.forget_window(&window);
            self.note_unmap(&window);
            self.space.unmap_elem(&window);
        }
        // Focus whatever is now on top, unless a panel has focus.
        if self.panels.active.is_none() {
            self.focus_top();
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        if let Err(err) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            tracing::warn!(?err, "failed to track popup");
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        // Popup grabs are not supported; popups still work, they just don't
        // get dismissed automatically by clicking elsewhere.
    }

    fn move_request(&mut self, surface: ToplevelSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let seat = Seat::from_resource(&seat).unwrap();
        let Some(start_data) = check_grab(&seat, surface.wl_surface(), serial) else {
            return;
        };
        let Some(window) = self.window_for_surface(surface.wl_surface()) else {
            return;
        };
        let initial_loc = self.space.element_location(&window).unwrap();
        let pointer = seat.get_pointer().unwrap();
        let grab = MoveGrab {
            start_data,
            window,
            initial_loc,
        };
        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: wl_seat::WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let seat = Seat::from_resource(&seat).unwrap();
        let Some(start_data) = check_grab(&seat, surface.wl_surface(), serial) else {
            return;
        };
        let Some(window) = self.window_for_surface(surface.wl_surface()) else {
            return;
        };
        let pointer = seat.get_pointer().unwrap();
        let grab = ResizeGrab::start(self, start_data, window, edges_from_xdg(edges));
        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    fn title_changed(&mut self, _surface: ToplevelSurface) {
        // Title is read at render time.
    }
}
delegate_xdg_shell!(State);

fn check_grab(
    seat: &Seat<State>,
    surface: &WlSurface,
    serial: Serial,
) -> Option<GrabStartData<State>> {
    let pointer = seat.get_pointer()?;
    if !pointer.has_grab(serial) {
        return None;
    }
    let start_data = pointer.grab_start_data()?;
    let (focus, _) = start_data.focus.as_ref()?;
    if !focus.id().same_client_as(&surface.id()) {
        return None;
    }
    Some(start_data)
}

impl State {
    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some(window) = self.window_for_surface(&root) else {
            return;
        };
        let out = self.output_geometry();
        let mut target = out;
        // Target rectangle relative to the window's surface origin.
        let window_loc = self.space.element_location(&window).unwrap() - window.geometry().loc;
        target.loc -= window_loc;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

// ---------------------------------------------------------------------------
// xdg-decoration: always server side
// ---------------------------------------------------------------------------

impl XdgDecorationHandler for State {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        if toplevel.is_initial_configure_sent() {
            // Force a configure so the decoration mode is always (re)sent.
            toplevel.send_configure();
        }
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        XdgDecorationHandler::new_decoration(self, toplevel);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        XdgDecorationHandler::new_decoration(self, toplevel);
    }
}
delegate_xdg_decoration!(State);

// KDE's server-decoration protocol, the only one GTK 3 (Firefox) asks: always server side
// too, whatever the client asks for.
impl KdeDecorationHandler for State {
    fn kde_decoration_state(&self) -> &KdeDecorationState {
        &self.kde_decoration_state
    }

    fn new_decoration(&mut self, _surface: &WlSurface, decoration: &OrgKdeKwinServerDecoration) {
        decoration.mode(KdeDecorationMode::Server);
    }

    fn request_mode(
        &mut self,
        _surface: &WlSurface,
        decoration: &OrgKdeKwinServerDecoration,
        _mode: WEnum<KdeDecorationMode>,
    ) {
        decoration.mode(KdeDecorationMode::Server);
    }
}
delegate_kde_decoration!(State);

// ---------------------------------------------------------------------------
// Protocols for nested compositors (gamescope): viewporter, presentation-time,
// relative pointer, pointer constraints, single-pixel buffers.
// ---------------------------------------------------------------------------

delegate_viewporter!(State);
delegate_presentation!(State);
delegate_relative_pointer!(State);
delegate_single_pixel_buffer!(State);

impl PointerConstraintsHandler for State {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        // Activate straight away if the pointer is already over the surface;
        // otherwise it activates when the pointer moves onto it (see input.rs).
        if pointer.current_focus().as_ref() == Some(surface) {
            self.maybe_activate_constraint(surface, pointer);
        }
    }

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        pointer: &PointerHandle<Self>,
        location: Point<f64, Logical>,
    ) {
        let active =
            with_pointer_constraint(surface, pointer, |c| c.is_some_and(|c| c.is_active()));
        if !active {
            return;
        }
        if let Some(origin) = self.surface_origin_under_pointer(surface) {
            let pos = origin + location;
            self.pointer_location = pos;
            pointer.set_location(pos);
        }
    }
}
delegate_pointer_constraints!(State);
