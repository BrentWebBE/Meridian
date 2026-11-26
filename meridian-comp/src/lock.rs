//! Locking the session (the ext-session-lock-v1 protocol).
//!
//! The lock screen is a separate program (`meridian-lock`). It asks us to
//! lock; from then on the compositor shows nothing but that program's
//! surfaces and sends input nowhere else, until it tells us to unlock.
//!
//! The compositor is what makes the lock trustworthy: even if the lock
//! program crashes, the session stays locked and blank. It is then started
//! again so that you can still get back in.

use std::time::{Duration, Instant};

use smithay::{
    desktop::utils::send_frames_surface_tree,
    output::Output,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::session_lock::{LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker},
};

use crate::state::Meridian;

/// How long to wait before restarting a lock program that went away.
const RESTART_AFTER: Duration = Duration::from_secs(2);

/// Lock-related state, kept in `Meridian::lock`.
#[derive(Default)]
pub struct Lock {
    /// True from the moment a lock program asks until it unlocks.
    pub locked: bool,
    /// The lock program's surface for each monitor.
    pub surfaces: Vec<(Output, LockSurface)>,
    /// When we last started the lock program, to avoid a restart loop.
    started: Option<Instant>,
}

impl SessionLockHandler for Meridian {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    /// A lock program wants to lock the session.
    fn lock(&mut self, confirmation: SessionLocker) {
        tracing::info!("session locked");
        self.set_overview(false);
        self.lock.locked = true;
        self.lock.surfaces.clear();
        // No window may keep receiving keys behind the lock.
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
        }
        // From the next frame on only lock surfaces are drawn, so we can
        // confirm: the client may now show its unlock dialog.
        confirmation.lock();
        self.ipc_broadcast(); // `locked` is part of the published state
        self.queue_redraw("lock");
        self.start_lock_watchdog();
    }

    fn unlock(&mut self) {
        tracing::info!("session unlocked");
        self.lock.locked = false;
        self.lock.surfaces.clear();
        let top = self.space.elements().next_back().cloned();
        self.focus_window(top.as_ref());
        self.queue_redraw("lock");
    }

    /// The lock program created its surface for one monitor.
    fn new_surface(&mut self, surface: LockSurface, wl_output: WlOutput) {
        let Some(output) = Output::from_resource(&wl_output) else { return };
        // Tell it to cover the whole monitor.
        if let Some(geometry) = self.space.output_geometry(&output) {
            surface.with_pending_state(|state| {
                state.size = Some((geometry.size.w as u32, geometry.size.h as u32).into());
            });
        }
        surface.send_configure();

        // The lock surface gets the keyboard, for the password.
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, Some(surface.wl_surface().clone()), SERIAL_COUNTER.next_serial());
        }
        self.lock.surfaces.retain(|(other, _)| *other != output);
        self.lock.surfaces.push((output, surface));
        self.queue_redraw("lock");
    }
}

impl Meridian {
    /// Start the lock program, unless the session is already locked.
    pub fn lock_session(&mut self) {
        if !self.lock.locked {
            self.lock.started = Some(Instant::now());
            self.spawn("meridian-lock", &[]);
        }
    }

    /// While locked: the lock surface under `pos`, with its position.
    pub fn lock_surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        let output = self.space.output_under(pos).next()?;
        let origin = self.space.output_geometry(output)?.loc;
        self.lock
            .surfaces
            .iter()
            .find(|(candidate, surface)| candidate == output && surface.alive())
            .map(|(_, surface)| (surface.wl_surface().clone(), origin.to_f64()))
    }

    /// While locked, check once a second that the lock program is still
    /// there, and bring it back if it died. A timer rather than a per-frame
    /// check, because a locked, idle screen draws no frames.
    fn start_lock_watchdog(&mut self) {
        use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
        let timer = Timer::from_duration(Duration::from_secs(1));
        let started = self.loop_handle.insert_source(timer, |_, _, state| {
            if !state.lock.locked {
                return TimeoutAction::Drop; // unlocked: the watchdog ends
            }
            let none_alive = !state.lock.surfaces.iter().any(|(_, surface)| surface.alive());
            let waited = state.lock.started.is_none_or(|started| started.elapsed() > RESTART_AFTER);
            if none_alive && waited {
                tracing::warn!("the lock screen is gone while the session is locked; starting it again");
                state.lock.started = Some(Instant::now());
                state.spawn("meridian-lock", &[]);
                state.queue_redraw("lock"); // show the blank backdrop meanwhile
            }
            TimeoutAction::ToDuration(Duration::from_secs(1))
        });
        if started.is_err() {
            tracing::error!("could not start the lock watchdog");
        }
    }

    /// Called after each frame: keep the lock surfaces animating.
    pub fn lock_after_frame(&mut self, output: &Output) {
        if !self.lock.locked {
            return;
        }
        let elapsed = self.start_time.elapsed();
        for (candidate, surface) in &self.lock.surfaces {
            if candidate == output && surface.alive() {
                send_frames_surface_tree(surface.wl_surface(), output, elapsed, Some(Duration::ZERO), |_, _| {
                    Some(output.clone())
                });
            }
        }
    }
}
