//! The compositor's state: one struct that every event callback gets `&mut` access to.

use std::{ffi::OsString, path::PathBuf, process::Command, sync::Arc, time::Duration, time::Instant};

use anyhow::Context;
use meridian_common::{
    config::{Action, Config, LayoutKind},
    theme::Themes,
};
use smithay::{
    backend::{
        egl::EGLDevice,
        renderer::{ImportDma, ImportMemWl, gles::GlesRenderer},
    },
    desktop::{
        LayerSurface, PopupManager, Space, Window, WindowSurfaceType, layer_map_for_output,
        utils::send_frames_surface_tree,
    },
    input::{
        Seat, SeatState,
        keyboard::{Keycode, XkbConfig},
        pointer::CursorImageStatus,
    },
    output::Output,
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction,
            channel::{Event as ChannelEvent, Sender, channel},
            generic::Generic,
        },
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
        },
    },
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        dmabuf::{DmabufFeedbackBuilder, DmabufGlobal, DmabufState},
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        session_lock::SessionLockManagerState,
        shell::{
            wlr_layer::{Layer, WlrLayerShellState},
            xdg::{XdgShellState, decoration::XdgDecorationState},
        },
        shm::ShmState,
        socket::ListeningSocketSource,
    },
};

use crate::{
    backend::{Backend, Scene},
    decorations::{self, TITLE_HEIGHT},
    effects::Effects,
    grabs::ResizeState,
    lock::Lock,
    ipc::IpcServer,
    keys::{self, Binding},
    layout::{self, Layout, Rect},
};

/// Terminals tried by "spawn-terminal" when the config does not name one.
const TERMINALS: &[&str] = &["foot", "alacritty", "kitty", "ghostty", "weston-terminal"];

pub struct Meridian {
    pub start_time: Instant,
    /// Name of our socket in `$XDG_RUNTIME_DIR`, e.g. "wayland-1".
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,
    /// Lets callbacks register new event sources (timers) later on.
    pub loop_handle: LoopHandle<'static, Meridian>,
    /// Lets any callback stop the event loop.
    pub loop_signal: LoopSignal,

    pub backend: Backend,
    pub config: Config,
    pub bindings: Vec<Binding>,

    /// A 2D plane holding the windows (with position and stacking order) and outputs.
    pub space: Space<Window>,
    /// All windows in the order they were opened. The space's own order
    /// changes whenever a window is raised, so tiling uses this list instead.
    pub windows: Vec<Window>,
    /// The active layout, as a trait object (see layout.rs).
    pub layout: Box<dyn Layout>,
    pub popups: PopupManager,

    /// The three moods' colors, from the themes file.
    pub themes: Themes,
    /// Sends decoded wallpaper images from the worker thread to the main loop.
    pub wallpaper_sender: Option<Sender<WallpaperImage>>,
    /// Compiled shaders for rounded corners and shadows (effects.rs).
    pub effects: Effects,
    /// Index of the space (virtual desktop) being shown, see overview.rs.
    pub active_space: usize,
    /// True while the overview is open.
    pub overview: bool,
    /// Super was pressed and nothing else since: releasing it opens Find.
    pub super_tap: bool,
    pub open_find_after_key: bool,

    /// The surface with keyboard focus, kept up to date by `focus_changed`.
    /// We keep our own copy because Smithay holds the keyboard's lock while
    /// it notifies us, so asking the keyboard at that moment would deadlock.
    pub focused_surface: Option<WlSurface>,

    /// What clients want the pointer to look like right now.
    pub cursor_status: CursorImageStatus,
    /// The interactive resize in progress, if any (see grabs.rs).
    pub resize: Option<ResizeState>,

    /// Session lock state, see lock.rs.
    pub lock: Lock,
    pub session_lock_state: SessionLockManagerState,

    /// Connections from shell programs (bar, dock), see ipc.rs.
    pub ipc: IpcServer,
    pub next_window_id: u32,
    /// Counts config reloads; shell programs re-read the file when it changes.
    pub config_generation: u32,
    /// Path of the IPC socket, passed to programs we start.
    pub ipc_socket: Option<PathBuf>,

    /// Keys whose press triggered a compositor shortcut. Their release must not
    /// reach the client either, or it would see a release without a press.
    pub suppressed_keys: Vec<Keycode>,

    // Smithay's per-protocol state. Each `XxxState::new` advertises a Wayland
    // "global" that clients can bind to; the matching trait impls are in `handlers`.
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub layer_shell_state: WlrLayerShellState,
    pub _xdg_decoration_state: XdgDecorationState,
    pub shm_state: ShmState,
    pub dmabuf_state: DmabufState,
    pub _dmabuf_global: DmabufGlobal,
    pub _output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Meridian>,
    pub data_device_state: DataDeviceState,

    /// A seat is one set of input devices (keyboard + pointer) and its focus.
    pub seat: Seat<Self>,
}

impl Meridian {
    pub fn new(
        event_loop: &mut EventLoop<'static, Self>,
        display: Display<Self>,
        mut backend: Backend,
        config: Config,
    ) -> anyhow::Result<Self> {
        let dh = display.handle();

        // wl_compositor: lets clients create surfaces (rectangles of pixels).
        let compositor_state = CompositorState::new::<Self>(&dh);
        // xdg_shell: turns surfaces into desktop windows ("toplevels") and popups.
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        // wlr-layer-shell: bar, dock and other desktop surfaces (handlers/layer_shell.rs).
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        // xdg-decoration: lets clients hand the title bar over to us (decorations.rs).
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        // ext-session-lock: lets the lock screen program lock the session (lock.rs).
        // The filter decides which clients may see the protocol: all of them,
        // as locking can only take access away.
        let session_lock_state = SessionLockManagerState::new::<Self, _>(&dh, |_client| true);
        // wl_shm: clients share pixel buffers through shared memory (CPU rendering).
        let shm_state = ShmState::new::<Self>(&dh, backend.renderer().shm_formats());
        // linux-dmabuf: clients share GPU buffers. Anything using OpenGL/Vulkan
        // (Alacritty, GTK4, browsers) needs this.
        let (dmabuf_state, dmabuf_global) = init_dmabuf(&dh, backend.renderer());
        // wl_output / xdg_output: tells clients about monitors.
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        // wl_data_device: clipboard and drag-and-drop.
        let data_device_state = DataDeviceState::new::<Self>(&dh);

        let mut seat_state = SeatState::new();
        let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, backend.seat_name());

        // Keyboard layout from the config; empty strings mean "system default".
        let keyboard = &config.keyboard;
        let xkb = XkbConfig {
            layout: &keyboard.layout,
            variant: &keyboard.variant,
            options: (!keyboard.options.is_empty()).then(|| keyboard.options.clone()),
            ..XkbConfig::default()
        };
        seat.add_keyboard(xkb, keyboard.repeat_delay, keyboard.repeat_rate)
            .context("failed to set up the keyboard: check [keyboard] in your config")?;
        seat.add_pointer();

        let themes = Themes::load();
        let mood = config.appearance.current_mood(current_hour());
        let effects = Effects::compile(backend.renderer(), themes.get(mood))?;
        let socket_name = init_wayland_socket(display, event_loop)?;
        let bindings = keys::parse_bindings(&config.keybindings, backend.mod_is_logo());
        let layout = layout::from_kind(config.layout, &config);

        Ok(Self {
            start_time: Instant::now(),
            socket_name,
            display_handle: dh,
            loop_handle: event_loop.handle(),
            loop_signal: event_loop.get_signal(),
            backend,
            config,
            bindings,
            space: Space::default(),
            windows: Vec::new(),
            layout,
            popups: PopupManager::default(),
            themes,
            wallpaper_sender: None,
            effects,
            active_space: 0,
            overview: false,
            super_tap: false,
            open_find_after_key: false,
            focused_surface: None,
            cursor_status: CursorImageStatus::default_named(),
            resize: None,
            lock: Lock::default(),
            session_lock_state,
            ipc: IpcServer::default(),
            next_window_id: 1,
            ipc_socket: None,
            config_generation: 0,
            suppressed_keys: Vec::new(),
            compositor_state,
            xdg_shell_state,
            layer_shell_state,
            _xdg_decoration_state: xdg_decoration_state,
            shm_state,
            dmabuf_state,
            _dmabuf_global: dmabuf_global,
            _output_manager_state: output_manager_state,
            seat_state,
            data_device_state,
            seat,
        })
    }

    // ---- windows and focus ------------------------------------------------

    /// The window whose main surface is `surface`, if any.
    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<Window> {
        self.windows
            .iter()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
            .cloned() // `Window` is a cheap reference-counted handle
    }

    /// The layer surface (bar, dock, ...) under a point, looking only at the
    /// given layers. Also returns the exact surface and its global position.
    fn layer_under(
        &self,
        pos: Point<f64, Logical>,
        layers: &[Layer],
    ) -> Option<(LayerSurface, WlSurface, Point<f64, Logical>)> {
        let output = self.space.output_under(pos).next()?;
        let output_loc = self.space.output_geometry(output)?.loc;
        let map = layer_map_for_output(output);
        // Layer positions are relative to their output.
        let local = pos - output_loc.to_f64();

        layers.iter().find_map(|layer| {
            let surface = map.layer_under(*layer, local)?;
            let layer_loc = map.layer_geometry(surface)?.loc;
            let (wl_surface, surface_loc) =
                surface.surface_under(local - layer_loc.to_f64(), WindowSurfaceType::ALL)?;
            Some((surface.clone(), wl_surface, (surface_loc + layer_loc + output_loc).to_f64()))
        })
    }

    /// The layer surface a click at `pos` would hit, if it is not covered by a window.
    pub fn layer_surface_under(&self, pos: Point<f64, Logical>) -> Option<LayerSurface> {
        if let Some((layer, ..)) = self.layer_under(pos, &[Layer::Overlay, Layer::Top]) {
            return Some(layer);
        }
        if self.space.element_under(pos).is_some() {
            return None;
        }
        self.layer_under(pos, &[Layer::Bottom, Layer::Background])
            .map(|(layer, ..)| layer)
    }

    /// The surface under a point, with that surface's position in the space.
    /// Stacking order, top to bottom: overlay and top layers (bar, dock),
    /// windows, then bottom and background layers (wallpaper).
    pub fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        // While locked nothing but the lock screen can be pointed at.
        if self.lock.locked {
            return self.lock_surface_under(pos);
        }

        if let Some((_, surface, loc)) = self.layer_under(pos, &[Layer::Overlay, Layer::Top]) {
            return Some((surface, loc));
        }

        // In the overview windows are thumbnails; clients get no pointer input.
        // Title bars and borders are ours as well.
        if self.overview || self.frame_hit(pos).is_some() {
            return None;
        }

        // A window consists of several surfaces (popups, subsurfaces), hence
        // the second lookup inside the window.
        if let Some((window, window_loc)) = self.space.element_under(pos) {
            return window
                .surface_under(pos - window_loc.to_f64(), WindowSurfaceType::ALL)
                .map(|(surface, surface_loc)| (surface, (surface_loc + window_loc).to_f64()));
        }

        self.layer_under(pos, &[Layer::Bottom, Layer::Background])
            .map(|(_, surface, loc)| (surface, loc))
    }

    /// Give `window` keyboard focus and raise it; `None` clears the focus.
    pub fn focus_window(&mut self, window: Option<&Window>) {
        // While locked the keyboard belongs to the lock screen alone.
        if self.lock.locked {
            return;
        }
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");

        match window {
            Some(window) => {
                // `true` also marks it "activated" and deactivates the others,
                // which is what makes clients draw a focused/unfocused title bar.
                self.space.raise_element(window, true);
                let surface = window.toplevel().map(|t| t.wl_surface().clone());
                keyboard.set_focus(self, surface, serial);
            }
            None => {
                for w in self.space.elements() {
                    w.set_activated(false);
                }
                keyboard.set_focus(self, None, serial);
            }
        }

        // State changes like "activated" only reach clients in a configure event.
        for w in &self.windows {
            if let Some(toplevel) = w.toplevel() {
                toplevel.send_pending_configure();
            }
        }
    }

    /// The window that currently has keyboard focus.
    pub fn focused_window(&self) -> Option<Window> {
        self.window_for_surface(self.focused_surface.as_ref()?)
    }

    /// Focus the next (`step = 1`) or previous (`step = -1`) window.
    fn focus_step(&mut self, step: isize) {
        // Only cycle through the windows of the space being shown.
        let windows = self.space_windows(self.active_space);
        let count = windows.len() as isize;
        if count == 0 {
            return;
        }
        let current = self
            .focused_window()
            .and_then(|focused| windows.iter().position(|w| *w == focused));
        let next = match current {
            // `rem_euclid` wraps around in both directions (-1 becomes count - 1).
            Some(index) => (index as isize + step).rem_euclid(count),
            None => 0,
        };
        self.focus_window(Some(&windows[next as usize]));
    }

    // ---- layout -----------------------------------------------------------

    /// The whole first monitor.
    pub fn output_area(&self) -> Option<Rect> {
        let output = self.space.outputs().next()?;
        self.space.output_geometry(output)
    }

    /// The part of the first monitor that windows may use: everything not
    /// reserved by the bar, dock or other layer surfaces.
    pub fn usable_area(&self) -> Option<Rect> {
        let output = self.space.outputs().next()?;
        let output_geo = self.space.output_geometry(output)?;
        let mut area = layer_map_for_output(output).non_exclusive_zone();
        area.loc += output_geo.loc; // the zone is relative to the output
        Some(area)
    }

    /// True when the layout decides window positions (so the user cannot drag them).
    pub fn layout_is_managed(&self) -> bool {
        self.layout.kind() != LayoutKind::Floating
    }

    /// Ask the active layout where windows go, and move/resize them accordingly.
    pub fn apply_layout(&mut self) {
        // Monitor sizes may have changed: let bars and docks re-anchor first.
        for output in self.space.outputs() {
            layer_map_for_output(output).arrange();
        }

        let Some(area) = self.usable_area() else { return };
        // Each space is arranged on its own; parked spaces are left alone.
        let windows = self.space_windows(self.active_space);
        let Some(rects) = self.layout.arrange(area, windows.len()) else {
            // Floating: windows stay where they are, except that a title
            // bar must never end up underneath the top bar (this happens
            // when a window opens before the bar has reserved its space).
            for window in &windows {
                let Some(location) = self.space.element_location(window) else { continue };
                let bar = if decorations::is_decorated(window) { TITLE_HEIGHT } else { 0 };
                let highest = area.loc.y + bar;
                if location.y < highest {
                    self.space.map_element(window.clone(), (location.x, highest + 8), false);
                }
            }
            return;
        };

        for (window, rect) in windows.iter().zip(rects) {
            // The tile is for the whole frame; the content gets what is
            // left under our title bar.
            let bar = if decorations::is_decorated(window) { TITLE_HEIGHT } else { 0 };
            let rect = Rect::new(
                (rect.loc.x, rect.loc.y + bar).into(),
                (rect.size.w, (rect.size.h - bar).max(1)).into(),
            );
            if let Some(toplevel) = window.toplevel() {
                // We cannot resize a client's window ourselves. We send the
                // size we want in a configure event and the client redraws.
                toplevel.with_pending_state(|state| {
                    state.size = Some(rect.size);
                    // "Tiled" tells the client to drop shadows and rounded
                    // corners on those edges and to obey the size exactly.
                    for edge in TILED_STATES {
                        state.states.set(edge);
                    }
                });
                toplevel.send_pending_configure();
            }
            self.space.map_element(window.clone(), rect.loc, false);
        }
    }

    pub fn set_layout(&mut self, kind: LayoutKind) {
        self.layout = layout::from_kind(kind, &self.config);
        tracing::info!("layout: {kind:?}");

        if !self.layout_is_managed() {
            // Back to floating: windows keep their place but are free again.
            for window in &self.windows {
                if let Some(toplevel) = window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        for edge in TILED_STATES {
                            state.states.unset(edge);
                        }
                    });
                    toplevel.send_pending_configure();
                }
            }
        }
        self.apply_layout();
        self.ipc_broadcast();
    }

    // ---- configuration ----------------------------------------------------

    /// Read the config file again and apply what can change while running:
    /// theme, layout, keybindings, and whatever the shell programs read.
    /// (Keyboard layout and output scale need a restart.)
    pub fn reload_config(&mut self) {
        let config = match Config::load() {
            Ok(config) => config,
            Err(err) => {
                // Keep running with the old settings rather than half-apply a broken file.
                tracing::error!("{err}\nkeeping the current configuration");
                return;
            }
        };

        self.bindings = keys::parse_bindings(&config.keybindings, self.backend.mod_is_logo());
        let layout_changed = config.layout != self.layout.kind();
        let new_layout = config.layout;
        self.config = config;
        self.themes = Themes::load();
        self.config_generation += 1;
        self.load_wallpaper();

        self.update_theme();
        if layout_changed {
            self.set_layout(new_layout); // also broadcasts
        }
        self.ipc_broadcast();
        tracing::info!("configuration reloaded");
    }

    /// Reload the config whenever its file changes, using inotify: the
    /// kernel makes a file descriptor readable when something happens in a
    /// watched folder, which fits the event loop like any other source.
    pub fn watch_config_files(&mut self) {
        use inotify::{Inotify, WatchMask};
        let Some(folder) = Config::path().and_then(|path| path.parent().map(PathBuf::from)) else {
            return;
        };
        let _ = std::fs::create_dir_all(&folder);

        let watch = || -> std::io::Result<Inotify> {
            let inotify = Inotify::init()?;
            // The folder, not the file: programs replace config files by
            // renaming a new one into place, which a file watch would miss.
            inotify
                .watches()
                .add(&folder, WatchMask::CLOSE_WRITE | WatchMask::MOVED_TO | WatchMask::DELETE)?;
            Ok(inotify)
        };
        let inotify = match watch() {
            Ok(inotify) => inotify,
            Err(err) => {
                tracing::warn!("cannot watch {}: {err}", folder.display());
                return;
            }
        };

        let inserted = self
            .loop_handle
            .insert_source(Generic::new(inotify, Interest::READ, Mode::Level), |_, inotify, state| {
                let mut buffer = [0u8; 4096];
                // Safety: we only read events; the inotify instance is not dropped.
                let events = unsafe { inotify.get_mut() }.read_events(&mut buffer);
                let ours = events.is_ok_and(|events| {
                    events
                        .filter_map(|event| event.name.map(|name| name.to_os_string()))
                        .any(|name| name == "config.toml" || name == "themes.toml")
                });
                if ours {
                    state.reload_config();
                }
                Ok(PostAction::Continue)
            });
        if inserted.is_err() {
            tracing::warn!("cannot watch the config folder");
        }
    }

    /// Give the keyboard to the layer surface with this namespace, if it
    /// accepts keyboard focus.
    fn focus_layer(&mut self, namespace: &str) {
        let surface = self.space.outputs().find_map(|output| {
            layer_map_for_output(output)
                .layers()
                .find(|layer| layer.namespace() == namespace && layer.can_receive_keyboard_focus())
                .map(|layer| layer.wl_surface().clone())
        });
        if let (Some(surface), Some(keyboard)) = (surface, self.seat.get_keyboard()) {
            keyboard.set_focus(self, Some(surface), SERIAL_COUNTER.next_serial());
        }
    }

    /// Apply the mood for the current settings and time of day.
    pub fn update_theme(&mut self) {
        let mood = self.config.appearance.current_mood(current_hour());
        let theme = self.themes.get(mood);
        // Also true when only colors changed (an edited themes file).
        if theme != self.effects.theme() {
            tracing::info!("mood: {}", mood.name());
            self.effects.set_theme(theme);
            self.ipc_broadcast();
        }
    }

    /// Set up the channel that brings decoded wallpaper images to the main loop.
    pub fn init_wallpaper(&mut self) {
        // A calloop channel is an event source: sending from another thread
        // wakes the event loop and runs this callback on the main thread.
        let (sender, receiver) = channel::<WallpaperImage>();
        let inserted = self.loop_handle.insert_source(receiver, |event, _, state| {
            if let ChannelEvent::Msg(image) = event {
                state
                    .effects
                    .set_wallpaper_image(state.backend.renderer(), &image.pixels, image.width, image.height);
            }
        });
        if inserted.is_ok() {
            self.wallpaper_sender = Some(sender);
            self.load_wallpaper();
        }
    }

    /// Start loading the wallpaper image named in the config, if any.
    /// Decoding a large photo takes a while, so it happens on a worker
    /// thread; the main loop must never wait for the disk.
    pub fn load_wallpaper(&mut self) {
        let path = self.config.appearance.wallpaper.trim().to_string();
        if path.is_empty() {
            self.effects.clear_wallpaper_image();
            return;
        }
        let Some(sender) = self.wallpaper_sender.clone() else { return };
        // "~/Pictures/x.jpg" is a shell convention; expand it ourselves.
        let path = match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
            (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
            _ => PathBuf::from(path),
        };

        std::thread::spawn(move || match image::open(&path) {
            Ok(image) => {
                let image = image.to_rgba8();
                let (width, height) = image.dimensions();
                let _ = sender.send(WallpaperImage {
                    pixels: image.into_raw(),
                    width,
                    height,
                });
            }
            Err(err) => tracing::error!("wallpaper {}: {err}", path.display()),
        });
    }

    /// Check once a minute whether the time of day calls for another theme.
    pub fn start_theme_clock(&self) {
        use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
        let timer = Timer::from_duration(Duration::from_secs(60));
        let started = self.loop_handle.insert_source(timer, |_, _, state| {
            state.update_theme();
            TimeoutAction::ToDuration(Duration::from_secs(60)) // fire again
        });
        if started.is_err() {
            tracing::warn!("could not start the theme clock");
        }
    }

    // ---- actions ----------------------------------------------------------

    /// Run an action from a keybinding.
    pub fn run_action(&mut self, action: Action) {
        // While locked, shortcuts and requests do nothing. (A screenshot
        // is allowed: it can only show the lock screen.)
        if self.lock.locked && action != Action::Screenshot {
            return;
        }
        match action {
            Action::Lock => self.lock_session(),
            Action::SpawnTerminal => self.spawn_terminal(),
            Action::Spawn(command_line) => {
                let mut parts = command_line.split_whitespace().map(String::from);
                if let Some(program) = parts.next() {
                    let args: Vec<String> = parts.collect();
                    self.spawn(&program, &args);
                }
            }
            Action::CloseWindow => {
                // Polite request; the client decides (it may ask to save first).
                if let Some(toplevel) = self.focused_window().as_ref().and_then(|w| w.toplevel()) {
                    toplevel.send_close();
                }
            }
            Action::Quit => self.loop_signal.stop(),
            Action::ToggleLayout => {
                let next = match self.layout.kind() {
                    LayoutKind::Floating => LayoutKind::Tiled,
                    LayoutKind::Tiled => LayoutKind::Floating,
                };
                self.set_layout(next);
            }
            Action::Screenshot => self.screenshot(),
            Action::FocusDock => self.focus_layer("meridian-dock"),
            Action::ToggleOverview => self.set_overview(!self.overview),
            // Spaces are numbered from 1 in the config.
            Action::Workspace(number) => self.switch_space((number as usize).saturating_sub(1)),
            Action::MoveToWorkspace(number) => self.move_focused_to_space((number as usize).saturating_sub(1)),
            Action::FocusNext => self.focus_step(1),
            Action::FocusPrevious => self.focus_step(-1),
        }
    }

    /// Start a program as a client of this compositor.
    pub fn spawn(&self, program: &str, args: &[String]) {
        let mut command = Command::new(resolve_program(program));
        command
            .args(args)
            .env("WAYLAND_DISPLAY", &self.socket_name)
            // We have no X server yet; without this, X11-capable apps would
            // open on the host desktop instead.
            .env_remove("DISPLAY");
        if let Some(socket) = &self.ipc_socket {
            command.env(meridian_common::ipc::SOCKET_ENV, socket);
        }

        match command.spawn() {
            // Wait for the child on a helper thread so it does not linger as a zombie.
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(err) => tracing::error!("could not start `{program}`: {err}"),
        }
    }

    /// Start the programs listed under `autostart` in the config.
    pub fn autostart(&self) {
        for command_line in &self.config.autostart {
            let mut parts = command_line.split_whitespace().map(String::from);
            if let Some(program) = parts.next() {
                let args: Vec<String> = parts.collect();
                self.spawn(&program, &args);
            }
        }
    }

    /// Save a picture of the first monitor to ~/Pictures/Screenshots.
    fn screenshot(&mut self) {
        let Some(output) = self.space.outputs().next().cloned() else { return };
        let Some(path) = screenshot_path() else {
            tracing::error!("screenshot: cannot find your Pictures folder");
            return;
        };
        let scene = Scene {
            space: &self.space,
            effects: &self.effects,
            focused: self.focused_window(),
            overview: self.overview_scene(),
            lock: self.lock.locked.then_some(self.lock.surfaces.as_slice()),
            cursor: None,
        };
        match crate::backend::screenshot(self.backend.renderer(), &output, &scene, &path) {
            Ok(()) => tracing::info!("screenshot saved to {}", path.display()),
            Err(err) => tracing::error!("screenshot failed: {err:#}"),
        }
    }

    pub fn spawn_terminal(&self) {
        if !self.config.terminal.is_empty() {
            return self.spawn(&self.config.terminal, &[]);
        }
        match TERMINALS.iter().find(|name| find_in_path(name).is_some()) {
            Some(terminal) => self.spawn(terminal, &[]),
            None => tracing::error!("no terminal found, tried: {}", TERMINALS.join(", ")),
        }
    }

    // ---- per-frame housekeeping -------------------------------------------

    /// Called by the backend after it drew `output`.
    pub fn after_frame(&mut self, output: &Output) {
        // Frame callbacks tell clients "now is a good time to draw the next
        // frame". Without them most clients stop updating.
        let elapsed = self.start_time.elapsed();
        for window in self.space.elements_for_output(output) {
            window.send_frame(output, elapsed, Some(Duration::ZERO), |_, _| Some(output.clone()));
        }
        {
            let mut layers = layer_map_for_output(output);
            for layer in layers.layers() {
                layer.send_frame(output, elapsed, Some(Duration::ZERO), |_, _| Some(output.clone()));
            }
            layers.cleanup(); // forget closed layer surfaces
        }
        if let CursorImageStatus::Surface(surface) = &self.cursor_status {
            send_frames_surface_tree(surface, output, elapsed, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        }

        self.lock_after_frame(output);

        // Forget closed windows and popups, then flush outgoing messages.
        self.space.refresh();
        self.popups.cleanup();
        let _ = self.display_handle.flush_clients();
    }
}

/// A decoded wallpaper: RGBA pixels, 4 bytes each.
pub struct WallpaperImage {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// The hour of the day (0-23) in local time.
fn current_hour() -> u32 {
    use chrono::Timelike;
    chrono::Local::now().hour()
}

/// The four xdg_toplevel "tiled" edge states.
const TILED_STATES: [xdg_toplevel::State; 4] = [
    xdg_toplevel::State::TiledLeft,
    xdg_toplevel::State::TiledRight,
    xdg_toplevel::State::TiledTop,
    xdg_toplevel::State::TiledBottom,
];

/// Prefer a Meridian program sitting next to our own binary. That makes
/// `cargo run` use the freshly built bar and dock instead of installed ones.
fn resolve_program(program: &str) -> PathBuf {
    if !program.contains('/')
        && let Ok(exe) = std::env::current_exe()
        && let Some(sibling) = exe.parent().map(|dir| dir.join(program))
        && sibling.is_file()
    {
        return sibling;
    }
    PathBuf::from(program)
}

/// `~/Pictures/Screenshots/meridian-<unix time>.png`, creating the folder.
fn screenshot_path() -> Option<PathBuf> {
    let pictures = std::env::var_os("XDG_PICTURES_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Pictures")))?;
    let dir = pictures.join("Screenshots");
    std::fs::create_dir_all(&dir).ok()?;
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    Some(dir.join(format!("meridian-{seconds}.png")))
}

fn find_in_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// Advertise linux-dmabuf with the formats our renderer can import.
fn init_dmabuf(dh: &DisplayHandle, renderer: &mut GlesRenderer) -> (DmabufState, DmabufGlobal) {
    let formats = renderer.dmabuf_formats();
    let mut dmabuf_state = DmabufState::new();

    // Newer clients (Mesa) want "feedback": which GPU device to allocate on.
    let render_node = EGLDevice::device_for_display(renderer.egl_context().display())
        .and_then(|device| device.try_get_render_node());

    let feedback = match render_node {
        Ok(Some(node)) => DmabufFeedbackBuilder::new(node.dev_id(), formats.clone())
            .build()
            .ok(),
        _ => None,
    };

    let global = match feedback {
        Some(feedback) => dmabuf_state.create_global_with_default_feedback::<Meridian>(dh, &feedback),
        None => {
            tracing::warn!("could not find the GPU render node; GPU clients may fail to start");
            dmabuf_state.create_global::<Meridian>(dh, formats)
        }
    };

    (dmabuf_state, global)
}

/// Create the socket clients connect to, and hook it into the event loop.
fn init_wayland_socket(
    display: Display<Meridian>,
    event_loop: &mut EventLoop<'static, Meridian>,
) -> anyhow::Result<OsString> {
    // Picks the first free name: wayland-1 when your desktop already owns wayland-0.
    let socket = ListeningSocketSource::new_auto().context("failed to create the Wayland socket")?;
    let socket_name = socket.socket_name().to_os_string();
    let handle = event_loop.handle();

    // Event source 1: a new client connected to the socket.
    handle
        .insert_source(socket, |client_stream, _, state| {
            // `Arc` is a thread-safe reference-counted pointer; the Wayland
            // library keeps this per-client data alive as long as the client.
            if let Err(err) = state
                .display_handle
                .insert_client(client_stream, Arc::new(ClientState::default()))
            {
                tracing::warn!("failed to add a client: {err}");
            }
        })
        .map_err(|err| anyhow::anyhow!("failed to watch the Wayland socket: {err}"))?;

    // Event source 2: an existing client sent requests. `display` moves into
    // this source (ownership transfer); from now on we only use `DisplayHandle`.
    handle
        .insert_source(
            Generic::new(display, Interest::READ, Mode::Level),
            |_, display, state| {
                // Safety: we never drop the display while the source is registered.
                unsafe { display.get_mut().dispatch_clients(state)? };
                Ok(PostAction::Continue)
            },
        )
        .map_err(|err| anyhow::anyhow!("failed to watch client messages: {err}"))?;

    Ok(socket_name)
}

/// Data attached to each connected client.
#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}
