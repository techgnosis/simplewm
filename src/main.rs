mod bus;
mod clock;
mod grabs;
mod handlers;
mod input;
mod network;
mod panel;
mod portal;
mod portal_check;
mod screenshot;
mod services;
mod sound;
mod state;
mod text;
mod udev;
mod ui;
mod winit;

use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

use crate::state::State;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();

    // Use the TTY backend when there is no host session to nest in (e.g. launched
    // from a bare virtual console); otherwise run nested.
    let host_session = ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"]
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty()));
    let tty = !host_session;

    let mut event_loop: EventLoop<'static, State> = EventLoop::try_new()?;
    let display: Display<State> = Display::new()?;
    let mut state = State::new(&mut event_loop, display, tty);

    if tty {
        udev::run(&mut event_loop, &mut state)?;
    } else {
        winit::run(&mut event_loop, &mut state)?;
    }

    tracing::info!(socket = ?state.socket_name, "simplewm running");

    event_loop.run(None, &mut state, |state| {
        state.space.refresh();
        state.prune_stack();
        state.popups.cleanup();
        let _ = state.display_handle.flush_clients();
    })?;

    udev::shutdown(&mut state);
    state.panels.services.shutdown();

    Ok(())
}

/// Log to stderr with a fixed filter (no RUST_LOG: simplewm reads no environment
/// variables for settings). A debug build (`make debug`) also logs simplewm's debug
/// messages and smithay's DRM and session details.
fn init_logging() {
    use tracing::Level;
    use tracing_subscriber::{filter::Targets, prelude::*};

    let debug = cfg!(debug_assertions);
    let mut filter = Targets::new()
        .with_target("simplewm", if debug { Level::DEBUG } else { Level::INFO })
        .with_target("smithay", Level::WARN);
    if debug {
        filter = filter
            .with_target("smithay::backend::drm", Level::DEBUG)
            .with_target("smithay::backend::session", Level::DEBUG);
    } else {
        // Hide drm::device::fd "Unable to become drm master": a false alarm under a
        // libseat session, where logind/seatd already made the fd master and the kernel
        // just refuses a repeat SET_MASTER from a process that didn't open it.
        filter = filter.with_target("smithay::backend::drm::device::fd", Level::ERROR);
    }
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(filter)
        .init();
}
