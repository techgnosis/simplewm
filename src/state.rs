use std::{
    ffi::OsString,
    sync::{Arc, Mutex},
    time::Instant,
};

use smithay::{
    backend::renderer::gles::GlesRenderer,
    desktop::{PopupManager, Space, Window, WindowSurfaceType},
    input::{Seat, SeatState, pointer::CursorImageStatus},
    output::Output,
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction, RegistrationToken,
            channel::{self, Sender},
            generic::Generic,
        },
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
        },
    },
    utils::{Clock, IsAlive, Logical, Monotonic, Point, Rectangle, SERIAL_COUNTER},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        dmabuf::{DmabufGlobal, DmabufState},
        output::OutputManagerState,
        pointer_constraints::PointerConstraintsState,
        presentation::PresentationState,
        relative_pointer::RelativePointerManagerState,
        selection::data_device::DataDeviceState,
        shell::{
            kde::decoration::KdeDecorationState,
            xdg::{XdgShellState, decoration::XdgDecorationState},
        },
        shm::ShmState,
        single_pixel_buffer::SinglePixelBufferState,
        socket::ListeningSocketSource,
        viewporter::ViewporterState,
    },
};

use crate::{
    panel::{EnvRow, PanelId, Panels},
    portal::PortalEvent,
    screenshot::SaveResult,
    services::{Service, ServiceEvent, Services, Status},
    ui::{self, Ui},
};

// There is only ever one Backend, so its size does not matter.
#[allow(clippy::large_enum_variant)]
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
    /// KDE's older decoration protocol: GTK 3 (Firefox) only asks this one.
    pub kde_decoration_state: KdeDecorationState,
    pub shm_state: ShmState,
    #[allow(dead_code)]
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<State>,
    pub data_device_state: DataDeviceState,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    /// Protocols that nested compositors like gamescope require. They need no
    /// handling beyond creating their globals (see handlers.rs).
    #[allow(dead_code)]
    pub extra_globals: (
        ViewporterState,
        PresentationState,
        RelativePointerManagerState,
        PointerConstraintsState,
        SinglePixelBufferState,
    ),
    /// Monotonic clock for wp_presentation timestamps.
    pub clock: Clock<Monotonic>,

    pub seat: Seat<State>,
    pub pointer_location: Point<f64, Logical>,
    pub cursor_status: CursorImageStatus,

    pub output: Option<Output>,
    pub ui: Ui,
    pub panels: Panels,
    /// Stacking order of client windows and panels, bottom to top.
    pub stack: Vec<StackEntry>,
    pub backend: Option<Backend>,
    pub windows_opened: u32,
    /// Windows numbered so far for their bar tabs.
    pub windows_tagged: u32,
    /// Backend details recorded at startup, shown in the Environment applet.
    pub startup_info: Vec<(String, String)>,
    /// Screenshot worker threads report back through this.
    pub screenshot_tx: Sender<SaveResult>,
    /// Desktop Portal check threads report back through this.
    pub portal_check_tx: Sender<Vec<EnvRow>>,
    /// The key held down in one of simplewm's own windows, and its repeat timer.
    pub key_repeat: Option<(u32, RegistrationToken)>,
    /// On a TTY simplewm is the session: it runs the services and is the portal.
    tty: bool,
    /// For (re)starting the portal when the session bus comes up.
    portal_tx: Sender<PortalEvent>,
}

impl State {
    /// `tty`: simplewm is the session, so it also starts the services (session bus,
    /// PipeWire, …) and becomes the desktop portal.
    pub fn new(
        event_loop: &mut EventLoop<'static, State>,
        display: Display<State>,
        tty: bool,
    ) -> Self {
        let start_time = Instant::now();
        let dh = display.handle();

        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let kde_decoration_state = KdeDecorationState::new::<Self>(
            &dh,
            smithay::reexports::wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration_manager::Mode::Server,
        );
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let mut seat_state = SeatState::new();
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let dmabuf_state = DmabufState::new();
        let clock = Clock::<Monotonic>::new();
        let extra_globals = (
            ViewporterState::new::<Self>(&dh),
            PresentationState::new::<Self>(&dh, clock.id() as u32),
            RelativePointerManagerState::new::<Self>(&dh),
            PointerConstraintsState::new::<Self>(&dh),
            SinglePixelBufferState::new::<Self>(&dh),
        );

        let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_keyboard(Default::default(), 200, 25)
            .expect("failed to initialize keyboard");
        seat.add_pointer();

        let socket_name = Self::init_wayland_listener(display, event_loop);
        tracing::info!(?socket_name, "listening on wayland socket");

        let screenshot_tx = report_to(event_loop, State::screenshot_saved);

        // The services' threads report back here.
        let service_tx = report_to(event_loop, State::service_event);
        let service_tx = Mutex::new(service_tx);
        let services = Services::new(Arc::new(move |ev| {
            if let Ok(tx) = service_tx.lock() {
                let _ = tx.send(ev);
            }
        }));

        // The Password window's sudo workers report back here.
        let mut panels = Panels::new(services);
        panels.sudo.tx = Some(report_to(event_loop, |state, done| {
            if state.panels.sudo_done(done) && state.panels.active.is_none() {
                state.focus_top();
            }
        }));

        // File chooser requests to simplewm's desktop portal (answered by the File Picker).
        // Nested, the host session's portal keeps the bus name. On a TTY the portal starts
        // once the session bus is up (see `ensure_portal`).
        let portal_tx = report_to(event_loop, State::portal_event);
        let portal_check_tx =
            report_to(event_loop, |state, rows| state.panels.set_portal_rows(rows));

        let mut state = Self {
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
            kde_decoration_state,
            shm_state,
            output_manager_state,
            seat_state,
            data_device_state,
            dmabuf_state,
            dmabuf_global: None,
            extra_globals,
            clock,
            seat,
            pointer_location: (100.0, 100.0).into(),
            cursor_status: CursorImageStatus::default_named(),
            output: None,
            ui: Ui::new(),
            panels,
            stack: Vec::new(),
            backend: None,
            windows_opened: 0,
            windows_tagged: 0,
            startup_info: Vec::new(),
            screenshot_tx,
            portal_check_tx,
            key_repeat: None,
            tty,
            portal_tx,
        };
        if tty {
            state.panels.services.autostart();
            state.ensure_portal();
        }
        state
    }

    /// An event from one of the services' threads.
    fn service_event(&mut self, ev: ServiceEvent) {
        let s = ev.service;
        self.panels.service_event(ev);
        if s == Service::Bus {
            self.ensure_portal();
        }
    }

    /// On a TTY, start the portal whenever the session bus is up and the portal isn't
    /// serving (at startup, and after the bus was stopped and started again).
    pub fn ensure_portal(&mut self) {
        if !self.tty {
            return;
        }
        match self.panels.services.status(Service::Bus) {
            Status::Running | Status::Elsewhere(_) => {
                if crate::portal::needs_start() {
                    crate::portal::start(self.portal_tx.clone());
                }
            }
            // Including Starting: this runs again when it's up.
            _ => crate::portal::no_bus(),
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

    /// The part of the output below the bar, where windows go.
    pub fn usable_area(&self) -> Rectangle<i32, Logical> {
        let mut area = self.output_geometry();
        area.loc.y += ui::BAR_HEIGHT;
        area.size.h -= ui::BAR_HEIGHT;
        area
    }

    /// Returns the window owning the given toplevel wl_surface.
    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<Window> {
        self.space
            .elements()
            .find(|w| {
                w.toplevel()
                    .map(|t| t.wl_surface() == surface)
                    .unwrap_or(false)
            })
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

    fn stack_raise(&mut self, entry: StackEntry) {
        self.stack.retain(|e| *e != entry);
        self.stack.push(entry);
    }

    /// Raise a client window to the top of the stack.
    pub fn raise_window(&mut self, window: &Window) {
        self.stack_raise(StackEntry::Window(window.clone()));
        self.space.raise_element(window, true);
    }

    /// Forget a client window that is about to be unmapped.
    pub fn note_unmap(&mut self, window: &Window) {
        self.stack
            .retain(|e| *e != StackEntry::Window(window.clone()));
    }

    /// Drop stack entries for windows that no longer exist.
    pub fn prune_stack(&mut self) {
        let space = &self.space;
        self.stack.retain(|e| match e {
            StackEntry::Window(w) => w.alive() && space.elements().any(|x| x == w),
            StackEntry::Panel(_) => true,
        });
    }

    /// Visible stacking order, top-most first.
    pub fn stack_top_down(&self) -> Vec<StackEntry> {
        // Windows in the space but not (yet) in the stack go at the bottom.
        let mut v: Vec<StackEntry> = self
            .space
            .elements()
            .filter(|w| !self.stack.contains(&StackEntry::Window((*w).clone())))
            .map(|w| StackEntry::Window(w.clone()))
            .collect();
        v.extend(
            self.stack
                .iter()
                .filter(|e| match e {
                    StackEntry::Window(w) => w.alive() && self.space.elements().any(|x| x == w),
                    StackEntry::Panel(id) => self.panels.is_open(*id),
                })
                .cloned(),
        );
        v.reverse();
        v
    }

    /// Focus whatever is on top of the stack (after something closed).
    pub fn focus_top(&mut self) {
        match self.stack_top_down().into_iter().next() {
            Some(StackEntry::Window(w)) => self.focus_window(Some(&w)),
            Some(StackEntry::Panel(id)) => self.focus_panel(id),
            None => self.focus_window(None),
        }
    }

    /// Open a panel and give it focus.
    pub fn show_panel(&mut self, id: PanelId) {
        let out = self.output_geometry();
        self.panels.show(id, out);
        self.focus_panel(id);
    }

    /// Give a panel focus: raise it and take keyboard focus away from client
    /// windows.
    pub fn focus_panel(&mut self, id: PanelId) {
        if self.panels.editing.map(|f| f.panel()) != Some(id) {
            self.panels.stop_editing();
        }
        self.panels.active = Some(id);
        self.stack_raise(StackEntry::Panel(id));
        for w in self.space.elements() {
            w.set_activated(false);
            if let Some(t) = w.toplevel() {
                t.send_pending_configure();
            }
        }
        let serial = SERIAL_COUNTER.next_serial();
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, None, serial);
        }
    }

    pub fn focus_window(&mut self, window: Option<&Window>) {
        let serial = SERIAL_COUNTER.next_serial();
        if let Some(window) = window {
            self.raise_window(window);
            self.panels.active = None;
            self.panels.stop_editing();
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
        let mut cmd = std::process::Command::new(command);
        // Apps find the session bus here when simplewm runs it (nobody set the variable).
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none()
            && let Some(bus) = crate::services::bus_address()
        {
            cmd.env("DBUS_SESSION_BUS_ADDRESS", bus);
        }
        if let Err(err) = cmd
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .env("XDG_CURRENT_DESKTOP", "simplewm")
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
        let area = self.usable_area();
        (
            area.loc.x + 60 + 32 * n,
            area.loc.y + ui::TITLE_HEIGHT + 30 + 32 * n,
        )
            .into()
    }
}

/// A channel that worker threads report back on: each message reaches `handle` on
/// the event loop.
fn report_to<T: 'static>(
    event_loop: &EventLoop<'static, State>,
    mut handle: impl FnMut(&mut State, T) + 'static,
) -> Sender<T> {
    let (tx, rx) = channel::channel::<T>();
    event_loop
        .handle()
        .insert_source(rx, move |event, _, state| {
            if let channel::Event::Msg(msg) = event {
                handle(state, msg);
            }
        })
        .expect("failed to add a channel to the event loop");
    tx
}

/// One entry in the stacking order: a client window or a panel.
#[derive(Clone, PartialEq)]
pub enum StackEntry {
    Window(Window),
    Panel(PanelId),
}

impl State {
    /// (Re)run the Desktop Portal checks; the rows arrive on `portal_check_tx`.
    pub fn refresh_portal_check(&mut self) {
        self.panels
            .set_portal_rows(crate::portal_check::pending_rows());
        crate::portal_check::spawn(self.portal_check_tx.clone());
    }

    /// Everything shown in the Environment applet: how simplewm was started.
    pub fn environment_rows(&self) -> Vec<EnvRow> {
        let item = |k: &str, v: String| EnvRow::Item(k.to_string(), v);
        let mut rows = vec![EnvRow::Header("Wayland".into())];

        let socket = self.socket_name.to_string_lossy().into_owned();
        rows.push(item("WAYLAND_DISPLAY (for clients)", socket.clone()));
        match std::env::var("XDG_RUNTIME_DIR") {
            Ok(dir) => {
                rows.push(item("XDG_RUNTIME_DIR", dir.clone()));
                rows.push(item("Socket path", format!("{dir}/{socket}")));
                rows.push(item("Socket lock file", format!("{dir}/{socket}.lock")));
            }
            Err(_) => rows.push(item("XDG_RUNTIME_DIR", "(not set)".into())),
        }
        for (k, v) in &self.startup_info {
            rows.push(item(k, v.clone()));
        }
        if let Some(output) = &self.output {
            let mode = output
                .current_mode()
                .map(|m| {
                    format!(
                        "{}x{} @ {:.2} Hz",
                        m.size.w,
                        m.size.h,
                        m.refresh as f64 / 1000.0
                    )
                })
                .unwrap_or_else(|| "(no mode)".into());
            rows.push(item("Output", format!("{} — {}", output.name(), mode)));
        }
        rows.push(item("wl_seat", "seat0 (keyboard, pointer)".into()));
        rows.push(item(
            "Settings file",
            crate::panel::store_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(unavailable)".into()),
        ));

        let mut vars: Vec<(String, String)> = std::env::vars_os()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect();
        vars.sort();
        rows.push(EnvRow::Header(format!(
            "Environment variables ({})",
            vars.len()
        )));
        rows.extend(vars.into_iter().map(|(k, v)| EnvRow::Item(k, v)));
        rows
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
