//! TTY backend: DRM/KMS output + libinput input through a libseat session.
//! Single GPU, single output (the first connected connector).

use std::time::Duration;

use smithay::{
    backend::{
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent,
            compositor::{DrmCompositor, FrameFlags},
            exporter::gbm::GbmFramebufferExporter,
        },
        egl::{EGLContext, EGLDevice, EGLDisplay},
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{ImportDma, gles::GlesRenderer},
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{all_gpus, primary_gpu},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            EventLoop, RegistrationToken,
            timer::{TimeoutAction, Timer},
        },
        drm::control::{Device as ControlDevice, ModeTypeFlags, connector},
        input::Libinput,
        rustix::fs::OFlags,
    },
    utils::{DeviceFd, Transform},
    wayland::dmabuf::DmabufFeedbackBuilder,
};

use crate::{
    state::{Backend, State},
    ui::CLEAR_COLOR,
};

type GbmDrmCompositor =
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

pub struct UdevData {
    pub session: LibSeatSession,
    pub renderer: GlesRenderer,
    drm: DrmDevice,
    compositor: GbmDrmCompositor,
    libinput: Libinput,
    output: Output,
    /// Event source for DRM vblanks. It holds a reference to the DRM device, so it
    /// must be removed before the device can be dropped (see [`shutdown`]).
    drm_token: RegistrationToken,
    /// A frame has been queued and we're waiting for its vblank.
    frame_pending: bool,
    /// A retry timer is scheduled.
    timer_scheduled: bool,
}

impl UdevData {
    pub fn change_vt(&mut self, vt: i32) {
        if let Err(err) = self.session.change_vt(vt) {
            tracing::error!(?err, vt, "failed to switch VT");
        }
    }
}

pub fn run(event_loop: &mut EventLoop<'static, State>, state: &mut State) -> Result<(), Box<dyn std::error::Error>> {
    let (mut session, notifier) = LibSeatSession::new()?;
    let seat_name = session.seat();

    // --- GPU ---------------------------------------------------------------
    let gpu_path = primary_gpu(&seat_name)?
        .or_else(|| all_gpus(&seat_name).ok()?.into_iter().next())
        .ok_or("no GPU found")?;
    tracing::info!(?gpu_path, "using GPU");

    let fd = session.open(
        &gpu_path,
        OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
    )?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let (mut drm, drm_notifier) = DrmDevice::new(fd.clone(), true)?;
    let gbm = GbmDevice::new(fd.clone())?;
    let egl_display = unsafe { EGLDisplay::new(gbm.clone())? };
    let egl_context = EGLContext::new(&egl_display)?;
    let renderer = unsafe { GlesRenderer::new(egl_context)? };

    // --- connector / crtc / mode -------------------------------------------
    let res = drm.resource_handles()?;
    let connector = res
        .connectors()
        .iter()
        .filter_map(|c| drm.get_connector(*c, true).ok())
        .find(|c| c.state() == connector::State::Connected)
        .ok_or("no connected monitor")?;
    let drm_mode = connector
        .modes()
        .iter()
        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .or_else(|| connector.modes().first())
        .copied()
        .ok_or("monitor has no modes")?;
    let crtc = connector
        .encoders()
        .iter()
        .filter_map(|e| drm.get_encoder(*e).ok())
        .flat_map(|enc| res.filter_crtcs(enc.possible_crtcs()))
        .next()
        .ok_or("no CRTC for monitor")?;
    tracing::info!(connector = ?connector.interface(), mode = ?drm_mode, "using output");

    let (phys_w, phys_h) = connector.size().unwrap_or((0, 0));
    let output = Output::new(
        format!("{:?}-{}", connector.interface(), connector.interface_id()),
        PhysicalProperties {
            size: (phys_w as i32, phys_h as i32).into(),
            subpixel: Subpixel::Unknown,
            make: "simplewm".into(),
            model: "drm".into(),
        },
    );
    let mode = Mode::from(drm_mode);
    let _global = output.create_global::<State>(&state.display_handle);
    output.change_current_state(Some(mode), Some(Transform::Normal), None, Some((0, 0).into()));
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));
    state.output = Some(output.clone());

    let surface = drm.create_surface(crtc, drm_mode, &[connector.handle()])?;
    let allocator = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
    let exporter = GbmFramebufferExporter::new(gbm.clone(), None);
    let render_formats = renderer.egl_context().dmabuf_render_formats().clone();
    let compositor = DrmCompositor::new(
        &output,
        surface,
        None,
        allocator,
        exporter,
        [Fourcc::Argb8888, Fourcc::Xrgb8888],
        render_formats,
        drm.cursor_size(),
        // No cursor-plane GBM device: amdgpu rejects smithay's cursor-plane buffers
        // (falling back to legacy AddFB, then failing the plane test), so the cursor
        // always ends up composited on the primary plane anyway. Skip the attempt.
        None,
    )
    .map_err(|e| format!("failed to create DRM compositor: {e:?}"))?;

    // --- dmabuf ------------------------------------------------------------
    let formats = renderer.dmabuf_formats();
    let render_node = EGLDevice::device_for_display(&egl_display).and_then(|d| d.try_get_render_node());
    let global = match render_node {
        Ok(Some(node)) => {
            let feedback = DmabufFeedbackBuilder::new(node.dev_id(), formats).build()?;
            state
                .dmabuf_state
                .create_global_with_default_feedback::<State>(&state.display_handle, &feedback)
        }
        _ => state.dmabuf_state.create_global::<State>(&state.display_handle, formats),
    };
    state.dmabuf_global = Some(global);

    state.startup_info.push(("Backend".into(), "TTY (DRM/KMS + libinput)".into()));
    state.startup_info.push(("Session".into(), "libseat".into()));
    state.startup_info.push(("Seat".into(), seat_name.clone()));
    state.startup_info.push(("GPU device".into(), gpu_path.display().to_string()));
    if let Ok(Some(node)) = &render_node {
        let path = node.dev_path().map(|p| p.display().to_string()).unwrap_or_else(|| format!("{node:?}"));
        state.startup_info.push(("Render node (dmabuf)".into(), path));
    }
    state.startup_info.push((
        "Connector".into(),
        format!("{:?}-{}", connector.interface(), connector.interface_id()),
    ));

    // --- input -------------------------------------------------------------
    let mut libinput = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&seat_name)
        .map_err(|_| "failed to assign libinput seat")?;
    let libinput_backend = LibinputInputBackend::new(libinput.clone());

    let handle = event_loop.handle();
    handle.insert_source(libinput_backend, |mut event, _, state| {
        if let InputEvent::DeviceAdded { device } = &mut event {
            let _ = device.config_tap_set_enabled(true);
        }
        state.process_input_event(event);
    })?;

    handle.insert_source(notifier, |event, _, state| {
        let Some(Backend::Udev(udev)) = state.backend.as_mut() else {
            return;
        };
        match event {
            SessionEvent::PauseSession => {
                tracing::info!("session paused");
                udev.libinput.suspend();
                udev.drm.pause();
            }
            SessionEvent::ActivateSession => {
                tracing::info!("session activated");
                if udev.libinput.resume().is_err() {
                    tracing::error!("failed to resume libinput");
                }
                if let Err(err) = udev.drm.activate(false) {
                    tracing::error!(?err, "failed to activate drm");
                }
                if let Err(err) = udev.compositor.reset_state() {
                    tracing::warn!(?err, "failed to reset drm compositor state");
                }
                udev.frame_pending = false;
                state.udev_render();
            }
        }
    })?;

    let drm_token = handle.insert_source(drm_notifier, |event, _, state| match event {
        DrmEvent::VBlank(_) => {
            if let Some(Backend::Udev(udev)) = state.backend.as_mut() {
                if let Err(err) = udev.compositor.frame_submitted() {
                    tracing::warn!(?err, "frame_submitted failed");
                }
                udev.frame_pending = false;
            }
            state.udev_render();
        }
        DrmEvent::Error(err) => tracing::error!(?err, "drm error"),
    })?;

    drm.activate(false).ok();

    state.backend = Some(Backend::Udev(UdevData {
        session,
        renderer,
        drm,
        compositor,
        libinput,
        output,
        drm_token,
        frame_pending: false,
        timer_scheduled: false,
    }));

    state.udev_render();
    Ok(())
}

/// Tear down the DRM device while the libseat session is still open.
///
/// The compositor goes first: dropping its surface disables our CRTC/planes while we
/// still hold DRM master. The device is then paused before it drops, which skips
/// smithay's "restore previous state" commit. That commit replays every property it
/// snapshotted at startup, read-only ones (EDID, plane type, ...) included, and the
/// kernel rejects any atomic commit that sets an immutable property with EINVAL, so it
/// can never succeed. It isn't needed anyway: when the seat closes and we lose DRM
/// master, the kernel's fbdev helper restores the console by itself.
pub fn shutdown(state: &mut State) {
    if let Some(Backend::Udev(udev)) = state.backend.take() {
        state.loop_handle.remove(udev.drm_token);
        let UdevData { session, renderer, mut drm, compositor, libinput, output, .. } = udev;
        drop(compositor);
        drm.pause();
        drop(drm);
        drop((renderer, libinput, output));
        drop(session);
    }
}

impl State {
    fn udev_render(&mut self) {
        let Some(Backend::Udev(mut udev)) = self.backend.take() else {
            return;
        };
        if udev.frame_pending || !udev.session.is_active() {
            self.backend = Some(Backend::Udev(udev));
            return;
        }

        let elements = self.render_elements(&mut udev.renderer);
        let mut queued = false;
        match udev
            .compositor
            .render_frame(&mut udev.renderer, &elements, CLEAR_COLOR, FrameFlags::DEFAULT)
        {
            Ok(result) => {
                if !result.is_empty {
                    match udev.compositor.queue_frame(()) {
                        Ok(()) => queued = true,
                        Err(err) => tracing::warn!(?err, "queue_frame failed"),
                    }
                }
            }
            Err(err) => tracing::warn!(?err, "render_frame failed"),
        }
        drop(elements);

        udev.frame_pending = queued;
        let need_timer = !queued && !udev.timer_scheduled;
        if need_timer {
            udev.timer_scheduled = true;
        }
        let output = udev.output.clone();
        self.backend = Some(Backend::Udev(udev));

        if need_timer {
            // Nothing changed this time; poll again shortly.
            self.loop_handle
                .insert_source(Timer::from_duration(Duration::from_millis(16)), |_, _, state| {
                    if let Some(Backend::Udev(udev)) = state.backend.as_mut() {
                        udev.timer_scheduled = false;
                    }
                    state.udev_render();
                    TimeoutAction::Drop
                })
                .ok();
        }

        self.send_frames(&output);
    }
}
