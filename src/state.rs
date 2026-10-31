use std::{ffi::OsString, sync::Arc, time::Instant};

use smithay::{
    backend::renderer::gles::GlesRenderer,
    desktop::{PopupManager, Space, Window, WindowSurfaceType},
    input::{Seat, SeatState, pointer::CursorImageStatus},
    output::Output,
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction, generic::Generic,
        },
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
        },
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        dmabuf::{DmabufGlobal, DmabufState},
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::xdg::{XdgShellState, decoration::XdgDecorationState},
        shm::ShmState,
        socket::ListeningSocketSource,
    },
};

use crate::ui::{self, Ui};

pub enum Backend {
    Winit(crate::winit::WinitData),
    Udev(crate::udev::UdevData),
}

pub struct State {
    pub start_time: Instant,
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, State>,
    pub loop_signal: LoopSignal,
    pub socket_name: OsString,

    pub space: Space<Window>,
    pub popups: PopupManager,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    #[allow(dead_code)]
    pub xdg_decoration_state: XdgDecorationState,
    pub shm_state: ShmState,
    #[allow(dead_code)]
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<State>,
    pub data_device_state: DataDeviceState,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,

    pub seat: Seat<State>,
    pub pointer_location: Point<f64, Logical>,
    pub cursor_status: CursorImageStatus,

    pub output: Option<Output>,
    pub ui: Ui,
    pub backend: Option<Backend>,
    pub windows_opened: u32,
}

impl State {
    pub fn new(event_loop: &mut EventLoop<'static, State>, display: Display<State>) -> Self {
        let start_time = Instant::now();
        let dh = display.handle();

        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let mut seat_state = SeatState::new();
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let dmabuf_state = DmabufState::new();

        let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_keyboard(Default::default(), 200, 25)
            .expect("failed to initialize keyboard");
        seat.add_pointer();

        let socket_name = Self::init_wayland_listener(display, event_loop);
        tracing::info!(?socket_name, "listening on wayland socket");

        Self {
            start_time,
            display_handle: dh,
            loop_handle: event_loop.handle(),
            loop_signal: event_loop.get_signal(),
            socket_name,
            space: Space::default(),
            popups: PopupManager::default(),
            compositor_state,
            xdg_shell_state,
            xdg_decoration_state,
            shm_state,
            output_manager_state,
            seat_state,
            data_device_state,
            dmabuf_state,
            dmabuf_global: None,
            seat,
            pointer_location: (100.0, 100.0).into(),
            cursor_status: CursorImageStatus::default_named(),
            output: None,
            ui: Ui::new(),
            backend: None,
            windows_opened: 0,
        }
    }

    fn init_wayland_listener(
        display: Display<State>,
        event_loop: &mut EventLoop<'static, State>,
    ) -> OsString {
        let listening_socket = ListeningSocketSource::new_auto().expect("failed to create socket");
        let socket_name = listening_socket.socket_name().to_os_string();
        let handle = event_loop.handle();

        handle
            .insert_source(listening_socket, move |client_stream, _, state| {
                if let Err(err) = state
                    .display_handle
                    .insert_client(client_stream, Arc::new(ClientState::default()))
                {
                    tracing::warn!(?err, "failed to insert client");
                }
            })
            .expect("failed to init wayland listening socket");

        handle
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, state| {
                    // Safety: we don't drop the display
                    unsafe {
                        display.get_mut().dispatch_clients(state).unwrap();
                    }
                    Ok(PostAction::Continue)
                },
            )
            .expect("failed to init wayland server source");

        socket_name
    }

    /// Full output rectangle in logical coordinates.
    pub fn output_geometry(&self) -> Rectangle<i32, Logical> {
        self.output
            .as_ref()
            .and_then(|o| self.space.output_geometry(o))
            .unwrap_or_else(|| Rectangle::from_size((1280, 800).into()))
    }

    /// Returns the window owning the given toplevel wl_surface.
    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<Window> {
        self.space
            .elements()
            .find(|w| w.toplevel().map(|t| t.wl_surface() == surface).unwrap_or(false))
            .cloned()
    }

    /// Surface under the given point inside a window, with its global origin.
    pub fn surface_under_in_window(
        &self,
        window: &Window,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let loc = self.space.element_location(window)?;
        let render_loc = loc - window.geometry().loc;
        window
            .surface_under(pos - render_loc.to_f64(), WindowSurfaceType::ALL)
            .map(|(s, p)| (s, (p + render_loc).to_f64()))
    }

    pub fn focus_window(&mut self, window: Option<&Window>) {
        let serial = SERIAL_COUNTER.next_serial();
        if let Some(window) = window {
            self.space.raise_element(window, true);
        }
        for w in self.space.elements() {
            let active = Some(w) == window;
            w.set_activated(active);
            if let Some(t) = w.toplevel() {
                t.send_pending_configure();
            }
        }
        let surface = window.and_then(|w| w.toplevel().map(|t| t.wl_surface().clone()));
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, surface, serial);
        }
    }

    pub fn focused_window(&self) -> Option<Window> {
        let keyboard = self.seat.get_keyboard()?;
        let focus = keyboard.current_focus()?;
        self.window_for_surface(&focus)
    }

    pub fn spawn(&self, command: &str) {
        tracing::info!(command, "spawning");
        if let Err(err) = std::process::Command::new(command)
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .env_remove("DISPLAY")
            .spawn()
        {
            tracing::error!(?err, command, "failed to spawn");
        }
    }

    /// Renderer accessor for protocol handlers (dmabuf import).
    pub fn renderer(&mut self) -> Option<&mut GlesRenderer> {
        match self.backend.as_mut()? {
            Backend::Winit(w) => Some(w.backend.renderer()),
            Backend::Udev(u) => Some(&mut u.renderer),
        }
    }

    /// Place a newly mapped window in a cascade inside the usable area.
    pub fn initial_window_location(&mut self) -> Point<i32, Logical> {
        let n = (self.windows_opened % 10) as i32;
        self.windows_opened += 1;
        let out = self.output_geometry();
        (
            out.loc.x + 60 + 32 * n,
            out.loc.y + ui::BAR_HEIGHT + ui::TITLE_HEIGHT + 30 + 32 * n,
        )
            .into()
    }
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}
