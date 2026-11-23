//! Meridian's dock: a floating pill at the bottom with the pinned apps and a
//! dot under every app that is running. It can be switched off in the config
//! (`[dock] mode = "off"`); Find then does the launching.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use gtk::{gdk, gio, glib, prelude::*};
use gtk4_layer_shell::{Edge, Layer, LayerShell};
use meridian_common::{
    config::{Config, DockMode},
    ipc::{Connection, Request, State},
    theme,
};

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

/// State shared between the IPC callback and button clicks.
struct Dock {
    row: gtk::Box,
    pinned: Vec<String>,
    /// Every installed app, by lowercase desktop id.
    installed: HashMap<String, gio::AppInfo>,
    connection: Option<Rc<RefCell<Connection>>>,
}

fn build_ui(app: &gtk::Application) {
    let config = Config::load().unwrap_or_else(|err| {
        eprintln!("meridian-dock: {err}; using defaults");
        Config::default()
    });
    if config.dock.mode == DockMode::Off {
        return; // no window: the application exits
    }

    load_css();

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .css_classes(["dock-window"])
        .build();

    // Anchored to the bottom edge only, so the compositor centers it.
    window.init_layer_shell();
    window.set_namespace(Some("meridian-dock"));
    window.set_layer(Layer::Top);
    window.set_anchor(Edge::Bottom, true);
    // Keep tiled windows out from under the dock.
    window.auto_exclusive_zone_enable();

    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .css_classes(["dock"])
        .build();
    window.set_child(Some(&row));


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
        row,
        pinned: config.dock.pinned.iter().map(|id| id.to_lowercase()).collect(),
        installed,
        connection: connection.clone(),
    });

    // Draw once with no windows, then again on every update from the compositor.
    let empty = State {
        windows: Vec::new(),
        layout: config.layout,
        active_space: 0,
        space_count: 1,
        overview: false,
    };
    dock.rebuild(&empty);

    if let Some(connection) = connection {
        let fd = connection.borrow().raw_fd();
        glib_unix::unix_fd_add_local(fd, glib::IOCondition::IN | glib::IOCondition::HUP, move |_, _| {
            // The borrow must end before `rebuild`, which may borrow again.
            let received = connection.borrow_mut().receive::<State>();
            match received {
                Ok(states) => {
                    if let Some(state) = states.last() {
                        dock.rebuild(state);
                    }
                    glib::ControlFlow::Continue
                }
                Err(_) => glib::ControlFlow::Break, // compositor went away
            }
        });
    }

    window.present();
}

fn load_css() {
    let css = format!("{}\n{}", theme::LIGHT.css_variables(), include_str!("style.css"));
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

impl Dock {
    /// Pinned apps that are installed, followed by running apps that are not pinned.
    fn apps(&self, state: &State) -> Vec<DockApp> {
        let mut apps: Vec<DockApp> = self
            .pinned
            .iter()
            .filter_map(|key| {
                let info = self.installed.get(key)?;
                Some(DockApp {
                    key: key.clone(),
                    info: Some(info.clone()),
                })
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

    /// Throw away all buttons and build them again for the new state.
    /// With a handful of apps this is simpler than updating in place.
    fn rebuild(self: &Rc<Self>, state: &State) {
        while let Some(child) = self.row.first_child() {
            self.row.remove(&child);
        }

        for app in self.apps(state) {
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

            let dot = gtk::Box::builder().css_classes(["dot"]).halign(gtk::Align::Center).build();
            if !windows.is_empty() {
                dot.add_css_class("running");
            }

            // `Rc::clone` is cheap: it copies a pointer and bumps a counter.
            let dock = Rc::clone(self);
            let info = app.info.clone();
            button.connect_clicked(move |_| dock.activate(info.as_ref(), &windows));

            let tile = gtk::Box::new(gtk::Orientation::Vertical, 0);
            tile.append(&button);
            tile.append(&dot);
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

    /// Click on an app: focus it if it runs (cycling through its windows),
    /// otherwise start it.
    fn activate(&self, info: Option<&gio::AppInfo>, windows: &[(u32, bool)]) {
        if !windows.is_empty() {
            // The window after the focused one, or the first.
            let focused = windows.iter().position(|(_, focused)| *focused);
            let next = focused.map_or(0, |index| (index + 1) % windows.len());
            self.send(&Request::FocusWindow { id: windows[next].0 });
        } else if let Some(info) = info
            && let Err(err) = info.launch(&[], None::<&gio::AppLaunchContext>)
        {
            eprintln!("meridian-dock: could not start {}: {err}", info.name());
        }
    }
}
