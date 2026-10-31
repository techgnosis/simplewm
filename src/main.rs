mod grabs;
mod handlers;
mod input;
mod panel;
mod screenshot;
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
        // smithay warnings, minus two that are false alarms under a libseat session:
        // - drm::device::fd "Unable to become drm master": logind/seatd already made the
        //   fd master; the kernel just refuses a repeat SET_MASTER from a process that
        //   didn't open the fd.
        // - drm::surface::atomic "Failed to destroy old mode property blob": on the first
        //   modeset there is no blob of ours to destroy.
        tracing_subscriber::fmt()
            .with_env_filter(
                "simplewm=info,smithay=warn,\
                 smithay::backend::drm::device::fd=error,\
                 smithay::backend::drm::surface::atomic=error",
            )
            .init();
    }

    // Use the TTY backend when asked to, or when there is no host session to nest in
    // (e.g. launched from a bare virtual console).
    let host_session = ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"]
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty()));
    let tty = std::env::args().any(|a| a == "--tty") || !host_session;

    let mut event_loop: EventLoop<'static, State> = EventLoop::try_new()?;
    let display: Display<State> = Display::new()?;
    let mut state = State::new(&mut event_loop, display);

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

    Ok(())
}
