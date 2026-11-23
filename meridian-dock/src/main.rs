//! Meridian's dock. It has three modes, chosen in Settings under
//! "Launching apps" (`[dock] mode` in the config):
//!
//! * `dock`: always visible, with the pinned apps and a dot under every
//!   app that is running;
//! * `auto-hide`: only a thin handle at the screen edge; the dock slides
//!   out when the pointer reaches it and hides again shortly after;
//! * `off` ("Find only"): no dock, just a pill that opens Find.
//!
//! The dock re-reads the config whenever the file changes, so Settings
//! takes effect at once.

use std::{cell::RefCell, collections::HashMap, rc::Rc, time::Duration};

use gtk::{gdk, gio, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use meridian_common::{
    config::{Action, Config, DockMode, DockPosition},
    ipc::{Connection, Request, State},
    shell, theme,
};

/// How long the auto-hide dock stays after the pointer has left it.
const HIDE_DELAY: Duration = Duration::from_millis(600);

fn main() -> glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id("org.meridian.Dock")
        .build();
    app.connect_activate(build_ui);
    app.run()
}

/// One entry in the dock.
struct DockApp {
    /// Lowercase desktop id without ".desktop"; compared with window app ids.
    key: String,
    info: Option<gio::AppInfo>,
}

/// Everything the callbacks share.
struct Dock {
    window: gtk::ApplicationWindow,
    /// Holds the revealer, the handle and the Find pill.
    root: gtk::Box,
    /// Slides the row of apps in and out (auto-hide).
    revealer: gtk::Revealer,
    /// The row of app tiles.
    row: gtk::Box,
    handle: gtk::Box,
    find_pill: gtk::Button,
    /// Every installed app, by lowercase desktop id.
    installed: HashMap<String, gio::AppInfo>,
    connection: Option<Rc<RefCell<Connection>>>,
    config: RefCell<Config>,
    /// The compositor's last update, kept to redraw after a config change.
    state: RefCell<State>,
    /// The pending "hide the dock" timer in auto-hide mode.
    hide_timer: RefCell<Option<glib::SourceId>>,
}

fn build_ui(app: &gtk::Application) {
    shell::install_css(include_str!("style.css"));

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .css_classes(["dock-window"])
        .build();
    window.init_layer_shell();
    window.set_namespace(Some("meridian-dock"));
    window.set_layer(Layer::Top);
    // Take the keyboard only when asked to (Mod+D, or a click).
    window.set_keyboard_mode(KeyboardMode::OnDemand);
    if shell::reduced_motion() {
        window.add_css_class("reduced-motion");
    }

    let row = gtk::Box::builder().css_classes(["dock"]).build();
    let revealer = gtk::Revealer::builder()
        .child(&row)
        // No sliding for people who asked for reduced motion.
        .transition_duration(if shell::reduced_motion() { 0 } else { 180 })
        .build();
    let handle = gtk::Box::builder()
        .css_classes(["handle"])
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();

    let pill_content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    pill_content.append(&gtk::Label::new(Some("Find apps, files, anything")));
    pill_content.append(&gtk::Label::builder().label("Super").css_classes(["chip"]).build());
    let find_pill = gtk::Button::builder()
        .child(&pill_content)
        .css_classes(["find-pill"])
        .build();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    window.set_child(Some(&root));

    let config = Config::load().unwrap_or_else(|err| {
        eprintln!("meridian-dock: {err}; using defaults");
        Config::default()
    });

    let installed = gio::AppInfo::all()
        .into_iter()
        .filter_map(|info| {
            let id = info.id()?.to_lowercase();
            Some((id.strip_suffix(".desktop")?.to_string(), info))
        })
        .collect();

    let connection = match Connection::connect() {
        Ok(connection) => Some(Rc::new(RefCell::new(connection))),
        Err(err) => {
            eprintln!("meridian-dock: not connected to the compositor ({err}); running apps are not shown");
            None
        }
    };

    let dock = Rc::new(Dock {
        window: window.clone(),
        root,
        revealer,
        row,
        handle,
        find_pill,
        installed,
        connection: connection.clone(),
        state: RefCell::new(State {
            windows: Vec::new(),
            layout: config.layout,
            active_space: 0,
            space_count: 1,
            overview: false,
            theme: String::new(),
            config_generation: 0,
        }),
        config: RefCell::new(config),
        hide_timer: RefCell::new(None),
    });

    dock.find_pill.connect_clicked({
        let dock = Rc::clone(&dock);
        move |_| dock.send(&Request::Action(Action::Spawn("meridian-launcher".into())))
    });

    // Auto-hide: show while the pointer is over us, hide a moment after it leaves.
    let pointer = gtk::EventControllerMotion::new();
    pointer.connect_enter({
        let dock = Rc::clone(&dock);
        move |_, _, _| dock.reveal(true)
    });
    pointer.connect_leave({
        let dock = Rc::clone(&dock);
        move |_| dock.hide_later()
    });
    dock.root.add_controller(pointer);

    // Keyboard: Escape hands the keyboard back to the window you were in.
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed({
        let dock = Rc::clone(&dock);
        move |_, key, _, _| {
            if key == gdk::Key::Escape {
                dock.send(&Request::ReturnFocus);
                dock.hide_later();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        }
    });
    window.add_controller(keys);
    // When the dock gets the keyboard (Mod+D), show it and focus the first app.
    window.connect_is_active_notify({
        let dock = Rc::clone(&dock);
        move |window| {
            if window.is_active() {
                dock.reveal(true);
                dock.row.child_focus(gtk::DirectionType::TabForward);
            }
        }
    });

    dock.apply_config();
    dock.rebuild();

    // Follow the compositor: running apps and the mood.
    if let Some(connection) = connection {
        let fd = connection.borrow().raw_fd();
        let dock = Rc::clone(&dock);
        glib_unix::unix_fd_add_local(fd, glib::IOCondition::IN | glib::IOCondition::HUP, move |_, _| {
            // The borrow must end before `rebuild`, which may borrow again.
            let received = connection.borrow_mut().receive::<State>();
            match received {
                Ok(states) => {
                    if let Some(state) = states.last() {
                        shell::apply_mood(&state.theme);
                        *dock.state.borrow_mut() = state.clone();
                        dock.rebuild();
                    }
                    glib::ControlFlow::Continue
                }
                Err(_) => glib::ControlFlow::Break, // compositor went away
            }
        });
    }

    // Follow the config file: mode, position, dots, pinned apps.
    shell::watch_config({
        let dock = Rc::clone(&dock);
        move || match Config::load() {
            Ok(config) => {
                *dock.config.borrow_mut() = config;
                dock.apply_config();
                dock.rebuild();
            }
            // Mid-edit the file may be invalid; keep what we have.
            Err(err) => eprintln!("meridian-dock: {err}; keeping the current settings"),
        }
    });

    window.present();
}

impl Dock {
    /// Arrange the window for the configured mode and position.
    fn apply_config(&self) {
        let (mode, position) = {
            let config = self.config.borrow();
            (config.dock.mode, config.dock.position)
        };

        // Which screen edge, and which way things are laid out along it.
        let (edge, vertical) = match position {
            DockPosition::Bottom => (Edge::Bottom, false),
            DockPosition::Left => (Edge::Left, true),
            DockPosition::Right => (Edge::Right, true),
        };
        for other in [Edge::Bottom, Edge::Left, Edge::Right, Edge::Top] {
            self.window.set_anchor(other, other == edge);
        }
        let along = if vertical { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal };
        let across = if vertical { gtk::Orientation::Horizontal } else { gtk::Orientation::Vertical };
        self.row.set_orientation(along);
        self.root.set_orientation(across);
        if vertical {
            self.window.add_css_class("vertical");
        } else {
            self.window.remove_css_class("vertical");
        }
        self.revealer.set_transition_type(match position {
            DockPosition::Bottom => gtk::RevealerTransitionType::SlideUp,
            DockPosition::Left => gtk::RevealerTransitionType::SlideRight,
            DockPosition::Right => gtk::RevealerTransitionType::SlideLeft,
        });

        // The handle sits between the dock and the screen edge.
        while let Some(child) = self.root.first_child() {
            self.root.remove(&child);
        }
        if position == DockPosition::Left {
            self.root.append(&self.handle);
            self.root.append(&self.revealer);
        } else {
            self.root.append(&self.revealer);
            self.root.append(&self.handle);
        }
        self.root.append(&self.find_pill);

        self.cancel_hide();
        self.revealer.set_visible(mode != DockMode::Off);
        self.handle.set_visible(mode == DockMode::AutoHide);
        self.find_pill.set_visible(mode == DockMode::Off);
        match mode {
            DockMode::Dock | DockMode::Off => {
                self.revealer.set_reveal_child(true);
                // Reserve our size so tiled windows stay clear of us.
                self.window.auto_exclusive_zone_enable();
            }
            DockMode::AutoHide => {
                self.revealer.set_reveal_child(false);
                // Reserve nothing: the dock slides over the windows.
                self.window.set_exclusive_zone(0);
            }
        }
    }

    fn auto_hide(&self) -> bool {
        self.config.borrow().dock.mode == DockMode::AutoHide
    }

    fn cancel_hide(&self) {
        if let Some(timer) = self.hide_timer.borrow_mut().take() {
            timer.remove();
        }
    }

    fn reveal(&self, show: bool) {
        self.cancel_hide();
        if self.auto_hide() {
            self.revealer.set_reveal_child(show);
        }
    }

    /// Hide the auto-hide dock after a short delay, unless the pointer returns.
    fn hide_later(self: &Rc<Self>) {
        if !self.auto_hide() {
            return;
        }
        self.cancel_hide();
        let dock = Rc::clone(self);
        let timer = glib::timeout_add_local_once(HIDE_DELAY, move || {
            // The timer has fired; forget it so nobody tries to cancel it.
            dock.hide_timer.borrow_mut().take();
            // Stay open while the dock has the keyboard.
            if !dock.window.is_active() {
                dock.revealer.set_reveal_child(false);
            }
        });
        *self.hide_timer.borrow_mut() = Some(timer);
    }

    /// Pinned apps that are installed, followed by running apps that are not pinned.
    fn apps(&self, state: &State) -> Vec<DockApp> {
        let mut apps: Vec<DockApp> = self
            .config
            .borrow()
            .dock
            .pinned
            .iter()
            .map(|id| id.to_lowercase())
            .filter_map(|key| {
                let info = self.installed.get(&key)?.clone();
                Some(DockApp { key, info: Some(info) })
            })
            .collect();

        for window in &state.windows {
            let key = window.app_id.to_lowercase();
            if !key.is_empty() && !apps.iter().any(|app| app.key == key) {
                apps.push(DockApp {
                    info: self.installed.get(&key).cloned(),
                    key,
                });
            }
        }
        apps
    }

    /// Throw away all tiles and build them again for the current state.
    /// With a handful of apps this is simpler than updating in place.
    fn rebuild(self: &Rc<Self>) {
        while let Some(child) = self.row.first_child() {
            self.row.remove(&child);
        }
        let state = self.state.borrow();
        let show_dots = self.config.borrow().dock.running_dots;
        let vertical = self.row.orientation() == gtk::Orientation::Vertical;

        for app in self.apps(&state) {
            // Ids of this app's windows, and whether one of them is focused.
            let windows: Vec<(u32, bool)> = state
                .windows
                .iter()
                .filter(|window| window.app_id.to_lowercase() == app.key)
                .map(|window| (window.id, window.focused))
                .collect();

            let icon = match app.info.as_ref().and_then(|info| info.icon()) {
                Some(icon) => gtk::Image::from_gicon(&icon),
                None => gtk::Image::from_icon_name("application-x-executable"),
            };
            icon.set_pixel_size(theme::DOCK_ICON_SIZE);

            let name = app
                .info
                .as_ref()
                .map(|info| info.name().to_string())
                .unwrap_or_else(|| app.key.clone());
            let button = gtk::Button::builder()
                .child(&icon)
                .css_classes(["tile"])
                .tooltip_text(name)
                .build();

            let dot = gtk::Box::builder()
                .css_classes(["dot"])
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .build();
            if show_dots && !windows.is_empty() {
                dot.add_css_class("running");
            }

            // `Rc::clone` is cheap: it copies a pointer and bumps a counter.
            let dock = Rc::clone(self);
            let info = app.info.clone();
            button.connect_clicked(move |_| dock.activate(info.as_ref(), &windows));

            // The dot goes on the side facing the screen edge.
            let tile = gtk::Box::new(
                if vertical { gtk::Orientation::Horizontal } else { gtk::Orientation::Vertical },
                0,
            );
            if self.config.borrow().dock.position == DockPosition::Left {
                tile.append(&dot);
                tile.append(&button);
            } else {
                tile.append(&button);
                tile.append(&dot);
            }
            self.row.append(&tile);
        }
    }

    fn send(&self, request: &Request) {
        if let Some(connection) = &self.connection
            && let Err(err) = connection.borrow_mut().send(request)
        {
            eprintln!("meridian-dock: could not reach the compositor: {err}");
        }
    }

    /// Click (or Enter) on an app: focus it if it runs, cycling through
    /// its windows; otherwise start it.
    fn activate(&self, info: Option<&gio::AppInfo>, windows: &[(u32, bool)]) {
        if !windows.is_empty() {
            // The window after the focused one, or the first.
            let focused = windows.iter().position(|(_, focused)| *focused);
            let next = focused.map_or(0, |index| (index + 1) % windows.len());
            self.send(&Request::FocusWindow { id: windows[next].0 });
        } else if let Some(info) = info {
            if let Err(err) = info.launch(&[], None::<&gio::AppLaunchContext>) {
                eprintln!("meridian-dock: could not start {}: {err}", info.name());
            }
            // The new window takes the keyboard when it opens.
            self.send(&Request::ReturnFocus);
        }
    }
}
