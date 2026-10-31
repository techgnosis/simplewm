mod grabs;
mod handlers;
mod input;
mod panel;
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
        tracing_subscriber::fmt().with_env_filter("simplewm=info,smithay=warn").init();
    }

    let tty = std::env::args().any(|a| a == "--tty");

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

    Ok(())
}
