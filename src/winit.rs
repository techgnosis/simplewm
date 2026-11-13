//! Nested backend: runs as a window inside another Wayland session.

use std::time::Duration;

use smithay::{
    backend::{
        egl::EGLDevice,
        renderer::{ImportDma, damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    desktop::utils::OutputPresentationFeedback,
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::EventLoop,
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        winit::dpi::LogicalSize, winit::window::Window as WinitWindow,
    },
    utils::{Monotonic, Rectangle, Transform},
    wayland::{dmabuf::DmabufFeedbackBuilder, presentation::Refresh},
};

use crate::{
    state::{Backend, State},
    ui::CLEAR_COLOR,
};

pub struct WinitData {
    pub backend: WinitGraphicsBackend<GlesRenderer>,
    pub damage_tracker: OutputDamageTracker,
}

pub fn run(
    event_loop: &mut EventLoop<'static, State>,
    state: &mut State,
) -> Result<(), Box<dyn std::error::Error>> {
    let attributes = WinitWindow::default_attributes()
        .with_title("simplewm")
        .with_inner_size(LogicalSize::new(1280.0, 800.0));
    let (mut backend, winit_loop) = winit::init_from_attributes::<GlesRenderer>(attributes)?;

    let mode = Mode {
        size: backend.window_size(),
        refresh: 60_000,
    };
    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "simplewm".into(),
            model: "winit".into(),
        },
    );
    let _global = output.create_global::<State>(&state.display_handle);
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));
    state.output = Some(output.clone());

    // dmabuf with default feedback so GPU clients (alacritty) can use EGL.
    let render_node = EGLDevice::device_for_display(backend.renderer().egl_context().display())
        .and_then(|device| device.try_get_render_node());
    let formats = backend.renderer().dmabuf_formats();
    match render_node {
        Ok(Some(node)) => match DmabufFeedbackBuilder::new(node.dev_id(), formats).build() {
            Ok(feedback) => {
                let global = state
                    .dmabuf_state
                    .create_global_with_default_feedback::<State>(&state.display_handle, &feedback);
                state.dmabuf_global = Some(global);
            }
            Err(err) => tracing::warn!(?err, "failed to build dmabuf feedback"),
        },
        _ => {
            let global = state
                .dmabuf_state
                .create_global::<State>(&state.display_handle, formats);
            state.dmabuf_global = Some(global);
        }
    }

    state.startup_info.push((
        "Backend".into(),
        "nested (winit window in the host session)".into(),
    ));
    state.startup_info.push((
        "Host WAYLAND_DISPLAY".into(),
        std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "(not set)".into()),
    ));
    if let Ok(Some(node)) = &render_node {
        let path = node
            .dev_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| format!("{node:?}"));
        state
            .startup_info
            .push(("Render node (dmabuf)".into(), path));
    }

    let damage_tracker = OutputDamageTracker::from_output(&output);
    state.backend = Some(Backend::Winit(WinitData {
        backend,
        damage_tracker,
    }));

    event_loop
        .handle()
        .insert_source(winit_loop, move |event, _, state| match event {
            WinitEvent::Resized { size, .. } => {
                let mode = Mode {
                    size,
                    refresh: 60_000,
                };
                output.change_current_state(Some(mode), None, None, None);
                output.set_preferred(mode);
                state.ui.menu = None;
            }
            WinitEvent::Input(event) => state.process_input_event(event),
            WinitEvent::Redraw => render(state, &output),
            WinitEvent::CloseRequested => state.loop_signal.stop(),
            WinitEvent::Focus(_) => {}
        })?;

    Ok(())
}

fn render(state: &mut State, output: &Output) {
    let Some(Backend::Winit(mut data)) = state.backend.take() else {
        return;
    };

    let size = data.backend.window_size();
    let damage = Rectangle::from_size(size);
    let rendered = {
        match data.backend.bind() {
            Ok((renderer, mut framebuffer)) => {
                let elements = state.render_elements(renderer);
                data.damage_tracker
                    .render_output(renderer, &mut framebuffer, 0, &elements, CLEAR_COLOR)
                    .map_err(|err| tracing::warn!(?err, "render failed"))
                    .is_ok()
            }
            Err(err) => {
                tracing::warn!(?err, "bind failed");
                false
            }
        }
    };
    if rendered {
        if let Err(err) = data.backend.submit(Some(&[damage])) {
            tracing::warn!(?err, "submit failed");
        } else {
            // No vblank timing from the host window, so report "now".
            let mut feedback = state.take_presentation_feedback(output);
            feedback.presented::<_, Monotonic>(
                state.clock.now(),
                Refresh::Unknown,
                0,
                wp_presentation_feedback::Kind::empty(),
            );
        }
    }
    data.backend.window().request_redraw();
    state.backend = Some(Backend::Winit(data));

    state.send_frames(output);
}

impl State {
    /// Collect the wp_presentation feedback requested by everything drawn on
    /// `output`, to be fired once the frame is on screen.
    pub fn take_presentation_feedback(&self, output: &Output) -> OutputPresentationFeedback {
        let mut feedback = OutputPresentationFeedback::new(output);
        for window in self.space.elements() {
            window.take_presentation_feedback(
                &mut feedback,
                |_, _| Some(output.clone()),
                |_, _| wp_presentation_feedback::Kind::empty(),
            );
        }
        feedback
    }

    pub fn send_frames(&mut self, output: &Output) {
        let time = self.start_time.elapsed();
        for window in self.space.elements() {
            window.send_frame(output, time, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        }
        if let smithay::input::pointer::CursorImageStatus::Surface(ref surface) = self.cursor_status
        {
            smithay::desktop::utils::send_frames_surface_tree(
                surface,
                output,
                time,
                Some(Duration::ZERO),
                |_, _| Some(output.clone()),
            );
        }
        self.space.refresh();
        self.popups.cleanup();
        let _ = self.display_handle.flush_clients();
    }
}
