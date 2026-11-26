//! Real-session backend: Meridian owns the screen, started from a TTY or a
//! login manager.
//!
//! The Linux pieces involved:
//!
//! * **libseat / logind** hands us device access without root, and tells us
//!   when the user switches to another virtual terminal (VT).
//! * **DRM/KMS** is the kernel API for monitors. A "connector" is a physical
//!   port, a "CRTC" is the scanout engine that feeds one monitor.
//! * **GBM** allocates the GPU buffers we render into and hand to KMS.
//! * **libinput** delivers keyboard, mouse and touchpad events.
//! * **udev** reports hotplug (monitor plugged in or removed).
//!
//! Phase 2 limitation: one GPU. Monitors wired to a second GPU stay dark.

use std::{collections::HashMap, path::PathBuf, time::Duration};

use anyhow::Context;
use smithay::{
    backend::{
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent,
            compositor::FrameFlags,
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        egl::{EGLContext, EGLDisplay},
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::gles::GlesRenderer,
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{UdevBackend, UdevEvent, all_gpus, primary_gpu},
    },
    output::{Mode, Output, PhysicalProperties, Scale},
    reexports::{
        calloop::{
            EventLoop,
            timer::{TimeoutAction, Timer},
        },
        drm::{
            Device as _,
            control::{ModeTypeFlags, connector, crtc},
        },
        input::{Device as InputDevice, Libinput},
        rustix::fs::OFlags,
        wayland_server::backend::GlobalId,
    },
    utils::DeviceFd,
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

use super::{Backend, CursorFrame, OutputElement, Scene, output_elements};
use crate::{cursor::Cursor, state::Meridian};

// Type aliases: short names for long generic types.
type Allocator = GbmAllocator<DrmDeviceFd>;
type Exporter = GbmFramebufferExporter<DrmDeviceFd>;
type OutputManager = DrmOutputManager<Allocator, Exporter, (), DrmDeviceFd>;
type KmsOutput = DrmOutput<Allocator, Exporter, (), DrmDeviceFd>;

/// Pixel formats we are willing to scan out, best first.
const COLOR_FORMATS: &[Fourcc] = &[Fourcc::Argb8888, Fourcc::Abgr8888];


pub struct Tty {
    pub session: LibSeatSession,
    pub renderer: GlesRenderer,
    libinput: Libinput,
    /// Owns the DRM device and coordinates all monitors on it.
    drm: OutputManager,
    /// Remembers which connectors were plugged in at the last scan.
    scanner: DrmScanner,
    /// One entry per active monitor.
    surfaces: HashMap<crtc::Handle, Surface>,
    cursor: Cursor,
    /// False while the user is on another VT; we must not touch the GPU then.
    active: bool,
}

struct Surface {
    output: Output,
    kms: KmsOutput,
    global: GlobalId,
    redraw: Redraw,
}

/// Where a monitor is in its draw cycle.
///
/// Nothing is drawn on a schedule. Something asks for a redraw
/// (`queue_redraw`: a client committed, the pointer moved, a setting
/// changed), we draw once, hand the frame to the kernel, and wait for the
/// monitor to show it (VBlank) before drawing again. That keeps us exactly
/// in step with the monitor's refresh rate and idle when nothing changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Redraw {
    /// Nothing to do.
    Idle,
    /// A redraw was requested and will run as soon as the event loop is free.
    Queued,
    /// A frame was handed to the kernel; waiting for the VBlank event.
    /// `again`: something changed meanwhile, so draw again right after.
    WaitingForVblank { again: bool },
    /// We drew but nothing had changed, so no frame was shown and no VBlank
    /// will come. A timer stands in for it, so clients still get their
    /// "time to draw the next frame" signal.
    WaitingForTimer { again: bool },
}

impl Tty {
    pub fn seat_name(&self) -> String {
        self.session.seat()
    }
}

/// Take over the session and the GPU. Monitors are set up later by
/// `tty_scan_connectors`, once the compositor state exists.
pub fn init(event_loop: &EventLoop<'static, Meridian>) -> anyhow::Result<Tty> {
    // --- session -----------------------------------------------------------
    let (mut session, session_events) = LibSeatSession::new().context(
        "could not open a seat session. Start Meridian from a text console (TTY) or a login \
         manager, not from inside another desktop without --winit",
    )?;
    let seat = session.seat();

    // --- GPU ---------------------------------------------------------------
    let gpu_path = pick_gpu(&seat)?;
    tracing::info!("using GPU {}", gpu_path.display());

    // The session opens the device for us; that is what avoids needing root.
    let fd = session
        .open(
            &gpu_path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .with_context(|| format!("could not open {}", gpu_path.display()))?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));

    let (drm_device, drm_events) = DrmDevice::new(fd.clone(), true).context("could not set up DRM/KMS")?;
    let gbm = GbmDevice::new(fd).context("could not set up GBM")?;

    // EGL + OpenGL ES on that GPU. `unsafe` marks calls whose safety the
    // compiler cannot check; here: the GBM device must outlive the display,
    // which holds because the display keeps its own reference.
    let egl_display = unsafe { EGLDisplay::new(gbm.clone()) }.context("could not create an EGL display")?;
    let egl_context = EGLContext::new(&egl_display).context("could not create an EGL context")?;
    let renderer = unsafe { GlesRenderer::new(egl_context) }.context("could not create the OpenGL renderer")?;

    let render_formats = renderer.egl_context().dmabuf_render_formats().clone();
    let drm = DrmOutputManager::new(
        drm_device,
        GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT),
        GbmFramebufferExporter::new(gbm.clone(), None.into()),
        Some(gbm),
        COLOR_FORMATS.iter().copied(),
        render_formats,
    );

    // --- input -------------------------------------------------------------
    let mut libinput = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&seat)
        .map_err(|()| anyhow::anyhow!("libinput could not use seat {seat}"))?;

    // --- event sources -----------------------------------------------------
    let handle = event_loop.handle();

    handle
        .insert_source(LibinputInputBackend::new(libinput.clone()), |mut event, _, state| {
            if let InputEvent::DeviceAdded { device } = &mut event {
                configure_input_device(device);
            }
            state.process_input_event(event);
        })
        .map_err(|err| anyhow::anyhow!("failed to watch input devices: {err}"))?;

    handle
        .insert_source(session_events, |event, _, state| state.tty_session_event(event))
        .map_err(|err| anyhow::anyhow!("failed to watch the session: {err}"))?;

    handle
        .insert_source(drm_events, |event, _, state| match event {
            // VBlank: the monitor finished showing our last frame.
            DrmEvent::VBlank(crtc) => state.tty_vblank(crtc),
            DrmEvent::Error(err) => tracing::error!("DRM error: {err}"),
        })
        .map_err(|err| anyhow::anyhow!("failed to watch DRM events: {err}"))?;

    let udev = UdevBackend::new(&seat).context("could not start udev monitoring")?;
    handle
        .insert_source(udev, |event, _, state| match event {
            // "Changed" fires when a monitor is plugged in or removed.
            UdevEvent::Changed { .. } => state.tty_scan_connectors(),
            UdevEvent::Added { path, .. } => {
                tracing::info!("GPU {} appeared; multi-GPU is not supported yet", path.display())
            }
            UdevEvent::Removed { .. } => {}
        })
        .map_err(|err| anyhow::anyhow!("failed to watch udev: {err}"))?;

    Ok(Tty {
        session,
        renderer,
        libinput,
        drm,
        scanner: DrmScanner::new(),
        surfaces: HashMap::new(),
        cursor: Cursor::load(),
        active: true,
    })
}

/// Which GPU drives the monitors? `MERIDIAN_DRM_DEVICE=/dev/dri/cardN`
/// overrides the automatic choice (useful on hybrid laptops).
fn pick_gpu(seat: &str) -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("MERIDIAN_DRM_DEVICE") {
        return Ok(PathBuf::from(path));
    }
    // The "primary" GPU is the one the firmware booted on, normally the one
    // wired to the laptop panel.
    if let Some(path) = primary_gpu(seat).context("could not list GPUs")? {
        return Ok(path);
    }
    all_gpus(seat)
        .context("could not list GPUs")?
        .into_iter()
        .next()
        .context("no GPU found")
}

fn configure_input_device(device: &mut InputDevice) {
    // Touchpads: tap to click. libinput's default is off.
    if device.config_tap_finger_count() > 0 {
        let _ = device.config_tap_set_enabled(true);
    }
}

/// Integer scale for a monitor: the configured value, or 2 on HiDPI panels.
fn pick_scale(configured: u32, width_px: i32, width_mm: u32) -> i32 {
    if configured > 0 {
        return configured.clamp(1, 4) as i32;
    }
    if width_mm == 0 {
        return 1; // the monitor did not report its size
    }
    let dpi = width_px as f64 / (width_mm as f64 / 25.4);
    if dpi >= 168.0 { 2 } else { 1 }
}

impl Meridian {
    /// Switch to another virtual terminal (Ctrl+Alt+F1...F12).
    pub fn change_vt(&mut self, vt: i32) {
        if let Backend::Tty(tty) = &mut self.backend
            && let Err(err) = tty.session.change_vt(vt)
        {
            tracing::warn!("could not switch to VT {vt}: {err}");
        }
    }

    /// Look for plugged-in or removed monitors and update our outputs.
    pub fn tty_scan_connectors(&mut self) {
        let Backend::Tty(tty) = &mut self.backend else { return };

        let scan = match tty.scanner.scan_connectors(tty.drm.device()) {
            Ok(scan) => scan,
            Err(err) => {
                tracing::warn!("could not scan monitors: {err}");
                return;
            }
        };

        for event in scan {
            match event {
                DrmScanEvent::Connected {
                    connector,
                    crtc: Some(crtc),
                } => self.tty_connect(connector, crtc),
                DrmScanEvent::Disconnected { crtc: Some(crtc), .. } => self.tty_disconnect(crtc),
                _ => {}
            }
        }

        self.apply_layout();
    }

    fn tty_connect(&mut self, connector: connector::Info, crtc: crtc::Handle) {
        let configured_scale = self.config.output.scale;
        let Backend::Tty(tty) = &mut self.backend else { return };

        let name = format!("{}-{}", connector.interface().as_str(), connector.interface_id());

        // A "mode" is resolution + refresh rate. Start from the monitor's
        // preferred mode, then take the fastest refresh rate it offers at
        // that resolution (many 120 Hz panels "prefer" their 60 Hz mode).
        let modes = connector.modes();
        let preferred = modes
            .iter()
            .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or(modes.first())
            .copied();
        let Some(drm_mode) = preferred.map(|preferred| {
            modes
                .iter()
                .filter(|mode| mode.size() == preferred.size())
                .max_by_key(|mode| mode.vrefresh())
                .copied()
                .unwrap_or(preferred)
        }) else {
            tracing::warn!("monitor {name} reports no modes, skipping it");
            return;
        };
        let mode = Mode::from(drm_mode);

        let (width_mm, height_mm) = connector.size().unwrap_or((0, 0));
        let output = Output::new(
            name.clone(),
            PhysicalProperties {
                size: (width_mm as i32, height_mm as i32).into(),
                subpixel: connector.subpixel().into(),
                make: "Unknown".into(),
                model: "Unknown".into(),
                serial_number: "Unknown".into(),
            },
        );

        // Place the new monitor to the right of the existing ones.
        let x = self
            .space
            .outputs()
            .filter_map(|o| self.space.output_geometry(o))
            .map(|geo| geo.loc.x + geo.size.w)
            .max()
            .unwrap_or(0);
        let scale = pick_scale(configured_scale, mode.size.w, width_mm);

        output.set_preferred(mode);
        output.change_current_state(Some(mode), None, Some(Scale::Integer(scale)), Some((x, 0).into()));

        // Planes are hardware layers. NVIDIA's overlay planes are known to
        // misbehave, so we do not offer them to the DRM compositor there.
        let device = tty.drm.device();
        let mut planes = match device.planes(&crtc) {
            Ok(planes) => planes,
            Err(err) => {
                tracing::warn!("monitor {name}: could not query planes: {err}");
                return;
            }
        };
        let is_nvidia = device
            .get_driver()
            .map(|driver| driver.name().to_string_lossy().to_lowercase().contains("nvidia"))
            .unwrap_or(false);
        if is_nvidia {
            planes.overlay.clear();
        }

        let kms = match tty.drm.lock().initialize_output::<_, OutputElement>(
            crtc,
            drm_mode,
            &[connector.handle()],
            &output,
            Some(planes),
            &mut tty.renderer,
            &DrmOutputRenderElements::default(),
        ) {
            Ok(kms) => kms,
            Err(err) => {
                tracing::error!("monitor {name}: could not set mode: {err}");
                return;
            }
        };

        tracing::info!(
            "monitor {name}: {}x{} @ {:.0} Hz, scale {scale}",
            mode.size.w,
            mode.size.h,
            mode.refresh as f64 / 1000.0
        );

        let global = output.create_global::<Meridian>(&self.display_handle);
        self.space.map_output(&output, (x, 0));
        tty.surfaces.insert(
            crtc,
            Surface {
                output,
                kms,
                global,
                redraw: Redraw::Queued,
            },
        );

        self.tty_render(crtc);
    }

    fn tty_disconnect(&mut self, crtc: crtc::Handle) {
        let Backend::Tty(tty) = &mut self.backend else { return };
        if let Some(surface) = tty.surfaces.remove(&crtc) {
            tracing::info!("monitor {} disconnected", surface.output.name());
            self.space.unmap_output(&surface.output);
            self.display_handle.remove_global::<Meridian>(surface.global);
        }
    }

    /// Ask for every monitor to be redrawn soon. Cheap to call often:
    /// requests made while a frame is in flight are merged into one.
    pub fn queue_redraw(&mut self, reason: &'static str) {
        self.stats.reason(reason);
        let tty = match &mut self.backend {
            Backend::Tty(tty) => tty,
            Backend::Winit(winit) => {
                winit.queue_redraw();
                return;
            }
        };
        for (crtc, surface) in tty.surfaces.iter_mut() {
            match &mut surface.redraw {
                Redraw::Idle => {
                    surface.redraw = Redraw::Queued;
                    let crtc = *crtc;
                    // `insert_idle` runs the closure once the event loop has
                    // handled everything that is pending right now, so many
                    // changes in one batch cause a single redraw.
                    self.loop_handle.insert_idle(move |state| state.tty_render(crtc));
                }
                Redraw::Queued => {}
                Redraw::WaitingForVblank { again } | Redraw::WaitingForTimer { again } => *again = true,
            }
        }
    }

    /// The monitor has shown our frame.
    fn tty_vblank(&mut self, crtc: crtc::Handle) {
        let Backend::Tty(tty) = &mut self.backend else { return };
        let Some(surface) = tty.surfaces.get_mut(&crtc) else { return };

        if let Err(err) = surface.kms.frame_submitted() {
            tracing::warn!("frame bookkeeping failed: {err}");
        }
        let again = matches!(surface.redraw, Redraw::WaitingForVblank { again: true });
        surface.redraw = Redraw::Idle;
        self.tty_frame_done(crtc, again);
    }

    /// The stand-in for VBlank after a frame in which nothing changed.
    fn tty_timer_fired(&mut self, crtc: crtc::Handle) {
        let Backend::Tty(tty) = &mut self.backend else { return };
        let Some(surface) = tty.surfaces.get_mut(&crtc) else { return };
        let Redraw::WaitingForTimer { again } = surface.redraw else {
            return; // the state moved on (for example the session was paused)
        };
        surface.redraw = Redraw::Idle;
        self.tty_frame_done(crtc, again);
    }

    /// A frame cycle ended: tell clients they may draw their next frame, and
    /// draw again ourselves if something changed in the meantime.
    fn tty_frame_done(&mut self, crtc: crtc::Handle, again: bool) {
        let Backend::Tty(tty) = &mut self.backend else { return };
        let Some(surface) = tty.surfaces.get_mut(&crtc) else { return };
        let output = surface.output.clone();
        if again {
            surface.redraw = Redraw::Queued;
        }

        self.after_frame(&output);
        if again {
            self.tty_render(crtc);
        }
    }

    /// Draw one monitor. Only runs when a redraw is queued for it.
    fn tty_render(&mut self, crtc: crtc::Handle) {
        let frame_started = self.stats.start_frame();
        let pointer_location = self.seat.get_pointer().map(|p| p.current_location()).unwrap_or_default();
        let background = self.effects.theme().sky.to_array();
        let focused = self.focused_window();
        let overview = self.overview_scene();

        // `self.backend`, `self.space` and `self.cursor_status` are different
        // fields, so Rust lets us borrow one mutably and the others immutably
        // at the same time.
        let Backend::Tty(tty) = &mut self.backend else { return };
        let Some(surface) = tty.surfaces.get_mut(&crtc) else { return };
        if surface.redraw != Redraw::Queued {
            return;
        }
        if !tty.active {
            // On another VT: forget the request, `ActivateSession` asks again.
            surface.redraw = Redraw::Idle;
            return;
        }

        let scene = Scene {
            space: &self.space,
            effects: &self.effects,
            focused,
            overview,
            lock: self.lock.locked.then_some(self.lock.surfaces.as_slice()),
            cursor: Some(CursorFrame {
                status: &self.cursor_status,
                image: &tty.cursor,
                location: pointer_location,
            }),
        };
        let elements = output_elements(&mut tty.renderer, &surface.output, &scene);

        let frame_flags = if self.config.effects.direct_scanout {
            FrameFlags::DEFAULT
        } else {
            FrameFlags::empty()
        };
        // `FrameFlags::empty()`: always composite with OpenGL. Direct scanout
        // (handing client buffers straight to the monitor) is faster but is
        // where driver bugs live, so it stays off until this is tested widely.
        let queued = match surface
            .kms
            .render_frame(&mut tty.renderer, &elements, background, frame_flags)
        {
            // `is_empty`: nothing changed since the last frame, nothing to show.
            Ok(frame) if frame.is_empty => false,
            Ok(_) => match surface.kms.queue_frame(()) {
                Ok(()) => true,
                Err(err) => {
                    tracing::warn!("could not queue frame: {err}");
                    false
                }
            },
            Err(err) => {
                tracing::warn!("could not render frame: {err}");
                false
            }
        };

        self.stats.end_frame(frame_started, !queued);
        if queued {
            surface.redraw = Redraw::WaitingForVblank { again: false };
        } else {
            // No frame was shown, so no VBlank will come. Wait one refresh
            // interval instead, then finish the cycle the same way.
            surface.redraw = Redraw::WaitingForTimer { again: false };
            let refresh_mhz = surface.output.current_mode().map_or(60_000, |mode| mode.refresh.max(1));
            let interval = Duration::from_secs_f64(1000.0 / refresh_mhz as f64);
            let armed = self.loop_handle.insert_source(Timer::from_duration(interval), move |_, _, state| {
                state.tty_timer_fired(crtc);
                TimeoutAction::Drop // one-shot
            });
            if armed.is_err() {
                tracing::error!("could not arm the frame timer");
                surface.redraw = Redraw::Idle;
            }
        }
    }

    /// The user switched away from (pause) or back to (activate) our VT.
    fn tty_session_event(&mut self, event: SessionEvent) {
        let Backend::Tty(tty) = &mut self.backend else { return };

        match event {
            SessionEvent::PauseSession => {
                tracing::info!("session paused");
                tty.active = false;
                tty.libinput.suspend();
                tty.drm.pause();
            }
            SessionEvent::ActivateSession => {
                tracing::info!("session resumed");
                if tty.libinput.resume().is_err() {
                    tracing::error!("could not resume input devices");
                }
                if let Err(err) = tty.drm.lock().activate(false) {
                    tracing::error!("could not re-activate the display: {err}");
                }
                tty.active = true;

                for surface in tty.surfaces.values_mut() {
                    surface.redraw = Redraw::Idle;
                }
                // Monitors may have changed while we were away.
                self.tty_scan_connectors();
                self.queue_redraw("resume");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::pick_scale;

    #[test]
    fn scale_follows_config_then_dpi() {
        assert_eq!(pick_scale(3, 1920, 500), 3);
        assert_eq!(pick_scale(0, 1920, 0), 1); // unknown size
        assert_eq!(pick_scale(0, 1920, 527), 1); // 24" full HD, ~93 dpi
        assert_eq!(pick_scale(0, 2880, 302), 2); // 14" 2.8K laptop, ~242 dpi
    }
}
