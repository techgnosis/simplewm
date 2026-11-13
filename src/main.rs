mod grabs;
mod handlers;
mod input;
mod network;
mod panel;
mod portal;
mod portal_check;
mod screenshot;
mod session_env;
mod sound;
mod state;
mod text;
mod ui;
mod udev;
mod winit;

use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

use crate::state::State;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    } else {
        // smithay warnings, minus drm::device::fd "Unable to become drm master": a false
        // alarm under a libseat session, where logind/seatd already made the fd master and
        // the kernel just refuses a repeat SET_MASTER from a process that didn't open it.
        tracing_subscriber::fmt()
            .with_env_filter("simplewm=info,smithay=warn,smithay::backend::drm::device::fd=error")
            .init();
    }

    // Use the TTY backend when there is no host session to nest in (e.g. launched
    // from a bare virtual console); otherwise run nested.
    let host_session = ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"]
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty()));
    let tty = !host_session;

    let mut event_loop: EventLoop<'static, State> = EventLoop::try_new()?;
    let display: Display<State> = Display::new()?;
    let mut state = State::new(&mut event_loop, display);

    if tty {
        udev::run(&mut event_loop, &mut state)?;
    } else {
        winit::run(&mut event_loop, &mut state)?;
    }

    tracing::info!(socket = ?state.socket_name, "simplewm running");

    // On a TTY we are the session, so let D-Bus-activated services (the desktop portal's
    // file chooser) find our display. Nested, that would hijack the host session's.
    if tty {
        session_env::export(&state.socket_name);
    }

    event_loop.run(None, &mut state, |state| {
        state.space.refresh();
        state.prune_stack();
        state.popups.cleanup();
        let _ = state.display_handle.flush_clients();
    })?;

    if tty {
        session_env::unexport();
    }
    udev::shutdown(&mut state);

    Ok(())
}
