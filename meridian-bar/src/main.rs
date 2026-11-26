//! Meridian's top bar: app-menu button and focused app on the left, status
//! icons and the clock on the right.
//!
//! It is an ordinary GTK 4 program. What makes it a bar is the layer-shell
//! protocol: instead of a normal window, the compositor gives us a strip
//! anchored to the top edge and keeps other windows out of it.

mod sni;
mod tray;

use std::{cell::RefCell, rc::Rc};

use gtk::{gio, glib, prelude::*};
use gtk4_layer_shell::{Edge, Layer, LayerShell};
use meridian_common::{
    config::Action,
    ipc::{Connection, Request, State},
    shell, theme,
};

fn main() -> glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id("org.meridian.Bar")
        .build();
    // GTK calls `activate` once the application is ready to show windows.
    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &gtk::Application) {
    load_css();

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .default_height(theme::BAR_HEIGHT)
        .css_classes(["bar"])
        .build();

    // Turn the window into a layer surface, stretched along the top edge.
    window.init_layer_shell();
    window.set_namespace(Some("meridian-bar"));
    window.set_layer(Layer::Top);
    for edge in [Edge::Top, Edge::Left, Edge::Right] {
        window.set_anchor(edge, true);
    }
    // Reserve our height so windows are placed below the bar.
    window.auto_exclusive_zone_enable();

    // ---- left: menu button and focused app --------------------------------
    // The round logo button opens Find.
    let menu_button = gtk::Button::builder()
        // The first letter of the product name.
        .label(&theme::BRAND[..1])
        .css_classes(["menu"])
        .valign(gtk::Align::Center)
        .tooltip_text("Find apps and files")
        .build();
    menu_button.connect_clicked(|_| open_launcher());
    let app_name = gtk::Label::builder().label("Desktop").css_classes(["app-name"]).build();

    // The focused app's menus would go here. Showing another program's menus
    // needs a protocol we do not speak yet, so this is a visual stub only.
    let app_menu = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(14)
        .css_classes(["app-menu"])
        .tooltip_text("App menus are not available yet")
        .visible(false)
        .build();
    for title in ["File", "Edit", "View", "Window"] {
        app_menu.append(&gtk::Label::new(Some(title)));
    }

    let left = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    left.append(&menu_button);
    left.append(&app_name);
    left.append(&app_menu);
    // Shown only when the compositor says it runs nested.
    let nested = gtk::Label::builder()
        .label("Nested dev mode")
        .css_classes(["nested"])
        .tooltip_text("Running as a window inside another desktop. This is not a full session.")
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    left.append(&nested);

    // ---- right: tray and clock --------------------------------------------
    let network = gtk::Image::new();
    let volume = gtk::Image::new();
    let battery = gtk::Image::new();
    let bell = tray::bell();
    let clock = gtk::Label::builder().css_classes(["clock"]).build();

    let right = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    right.add_css_class("status");
    // Icons from background apps come first, then our own status icons.
    right.append(&sni::tray());
    right.append(&network);
    right.append(&volume);
    right.append(&battery);
    right.append(&bell);
    right.append(&clock);

    tray::watch_network(&network);
    tray::watch_volume(&volume);

    let content = gtk::CenterBox::builder().css_classes(["bar-content"]).build();
    // ---- center: one pill per space (virtual desktop) -------------------
    let spaces = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .css_classes(["spaces"])
        .valign(gtk::Align::Center)
        .build();

    content.set_start_widget(Some(&left));
    content.set_center_widget(Some(&spaces));
    content.set_end_widget(Some(&right));
    window.set_child(Some(&content));

    // ---- keep things up to date -------------------------------------------
    update_clock(&clock);
    // `clone!` with `#[weak]` gives the closure a weak reference: the timer
    // does not keep the label alive, and stops once the label is gone.
    glib::timeout_add_seconds_local(
        1,
        glib::clone!(
            #[weak]
            clock,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                update_clock(&clock);
                glib::ControlFlow::Continue
            }
        ),
    );

    update_battery(&battery);
    glib::timeout_add_seconds_local(
        30,
        glib::clone!(
            #[weak]
            battery,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                update_battery(&battery);
                glib::ControlFlow::Continue
            }
        ),
    );

    watch_compositor(app_name, app_menu, spaces, nested);

    window.present();
}

/// Theme tokens first, then our own stylesheet that refers to them.
fn load_css() {
    // Theme variables plus our stylesheet; re-themed when the mood changes.
    shell::install_css(include_str!("style.css"));
}

fn update_clock(label: &gtk::Label) {
    // e.g. "Mon 23 Nov  16:22"
    if let Ok(text) = glib::DateTime::now_local().and_then(|now| now.format("%a %-e %b  %H:%M")) {
        label.set_label(&text);
    }
}

/// Read the battery from sysfs. Desktops without a battery hide the icon.
fn update_battery(image: &gtk::Image) {
    let Some((percent, charging)) = read_battery() else {
        image.set_visible(false);
        return;
    };
    // Icon names from the standard icon theme go in steps of 10.
    let level = (percent / 10) * 10;
    let suffix = if charging { "-charging" } else { "" };
    image.set_icon_name(Some(&format!("battery-level-{level}{suffix}-symbolic")));
    image.set_tooltip_text(Some(&format!("Battery {percent}%")));
    image.set_visible(true);
}

fn read_battery() -> Option<(u32, bool)> {
    let supplies = std::fs::read_dir("/sys/class/power_supply").ok()?;
    for entry in supplies.flatten() {
        let path = entry.path();
        let kind = std::fs::read_to_string(path.join("type")).unwrap_or_default();
        if kind.trim() != "Battery" {
            continue;
        }
        let Some(percent) = std::fs::read_to_string(path.join("capacity"))
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
        else {
            continue; // e.g. a wireless mouse without a capacity value
        };
        let status = std::fs::read_to_string(path.join("status")).unwrap_or_default();
        return Some((percent.min(100), status.trim() == "Charging"));
    }
    None
}

/// Ask the compositor to start (or toggle) Find. Going through the
/// compositor means it gets the same environment as other programs.
fn open_launcher() {
    send(&Request::Action(Action::Spawn("meridian-launcher".into())));
}

/// Send one request to the compositor.
fn send(request: &Request) {
    let sent = Connection::connect().and_then(|mut connection| connection.send(request));
    if let Err(err) = sent {
        eprintln!("meridian-bar: could not reach the compositor: {err}");
    }
}

/// Rebuild the space pills: "1 2 3 4", the active one filled in.
fn show_spaces(spaces: &gtk::Box, state: &State) {
    while let Some(child) = spaces.first_child() {
        spaces.remove(&child);
    }
    for index in 0..state.space_count {
        let number = index + 1;
        let pill = gtk::Button::builder()
            .label(number.to_string())
            .css_classes(["space"])
            .valign(gtk::Align::Center)
            .tooltip_text(format!("Space {number}"))
            .build();
        if index == state.active_space {
            pill.add_css_class("active");
        }
        // A dot of emphasis for spaces that have windows.
        if state.windows.iter().any(|window| window.space == index) {
            pill.add_css_class("occupied");
        }
        pill.connect_clicked(move |_| send(&Request::Action(Action::Workspace(number as u8))));
        spaces.append(&pill);
    }
}

/// Show the focused application's name and the spaces, as reported by the compositor.
fn watch_compositor(app_name: gtk::Label, app_menu: gtk::Box, spaces: gtk::Box, nested: gtk::Label) {
    let connection = match Connection::connect() {
        Ok(connection) => connection,
        Err(err) => {
            eprintln!("meridian-bar: not connected to the compositor ({err}); the app name stays empty");
            return;
        }
    };
    let fd = connection.raw_fd();
    // `Rc<RefCell<..>>`: shared ownership (Rc) of a value we need to mutate
    // (RefCell) from inside a callback. Single-threaded, so no locking.
    let connection = Rc::new(RefCell::new(connection));

    // GLib calls this whenever the socket has data, on the main thread.
    glib_unix::unix_fd_add_local(fd, glib::IOCondition::IN | glib::IOCondition::HUP, move |_, _| {
        match connection.borrow_mut().receive::<State>() {
            Ok(states) => {
                // Only the newest snapshot matters.
                if let Some(state) = states.last() {
                    shell::apply_mood(&state.theme);
                    app_name.set_label(&focused_app_name(state));
                    // Menus only make sense when an app is focused.
                    app_menu.set_visible(state.windows.iter().any(|window| window.focused));
                    show_spaces(&spaces, state);
                    nested.set_visible(state.nested);
                }
                glib::ControlFlow::Continue
            }
            Err(_) => glib::ControlFlow::Break, // compositor went away
        }
    });
}

fn focused_app_name(state: &State) -> String {
    let Some(window) = state.windows.iter().find(|window| window.focused) else {
        return "Desktop".into();
    };
    // Prefer the human name from the app's .desktop file ("Files" rather
    // than "org.gnome.Nautilus").
    let wanted = format!("{}.desktop", window.app_id).to_lowercase();
    gio::AppInfo::all()
        .into_iter()
        .find(|info| info.id().is_some_and(|id| id.to_lowercase() == wanted))
        .map(|info| info.name().to_string())
        .unwrap_or_else(|| {
            if window.app_id.is_empty() {
                window.title.clone()
            } else {
                window.app_id.clone()
            }
        })
}
