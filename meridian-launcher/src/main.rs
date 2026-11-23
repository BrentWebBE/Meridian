//! Meridian's app menu: search field, pinned apps, all apps, recent files
//! and a power menu, opening above the dock.
//!
//! The program stays running in the background after its first start.
//! Starting it again (from the dock button, the bar, or Mod+Space) reaches the
//! running copy and toggles the menu, so opening it is instant.

mod apps;

use std::{cell::RefCell, rc::Rc};

use gtk::{gdk, gio, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use meridian_common::{
    config::{Action, Config},
    ipc::{Connection, Request, State},
    theme,
};

use crate::apps::AppEntry;

const PANEL_WIDTH: i32 = 470;
const PANEL_HEIGHT: i32 = 500;
const COLUMNS: u32 = 5;

fn main() -> glib::ExitCode {
    // A GTK application id makes the program single-instance: a second
    // start does not open a second copy but sends "activate" to the first.
    let app = gtk::Application::builder()
        .application_id("org.meridian.Launcher")
        .build();

    app.connect_activate(|app| match app.windows().first() {
        // Already running: toggle.
        Some(window) if window.is_visible() => window.set_visible(false),
        Some(window) => window.present(),
        None => build_ui(app).present(),
    });

    // Without this GTK would quit as soon as the menu is hidden.
    let _keep_running = app.hold();
    app.connect_startup(quit_with_compositor);
    app.run()
}

/// Quit when the compositor goes away. This program idles in the background
/// with no window, so it would otherwise outlive the session.
fn quit_with_compositor(app: &gtk::Application) {
    let Ok(connection) = Connection::connect() else {
        return; // started by hand outside Meridian: nothing to watch
    };
    let fd = connection.raw_fd();
    let connection = RefCell::new(connection);
    let app = app.clone();
    glib_unix::unix_fd_add_local(fd, glib::IOCondition::IN | glib::IOCondition::HUP, move |_, _| {
        // We ignore the window updates; only a closed socket matters.
        match connection.borrow_mut().receive::<State>() {
            Ok(_) => glib::ControlFlow::Continue,
            Err(_) => {
                app.quit();
                glib::ControlFlow::Break
            }
        }
    });
}

fn build_ui(app: &gtk::Application) -> gtk::ApplicationWindow {
    load_css();

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .css_classes(["launcher-window"])
        .build();

    // Cover the whole area between bar and dock with a transparent surface.
    // The visible panel sits in its bottom-left corner; a click anywhere
    // else lands on the transparent part and closes the menu.
    window.init_layer_shell();
    window.set_namespace(Some("meridian-launcher"));
    window.set_layer(Layer::Top);
    for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
        window.set_anchor(edge, true);
    }
    // Ask the compositor for all keyboard input while the menu is open.
    window.set_keyboard_mode(KeyboardMode::Exclusive);

    let config = Config::load().unwrap_or_else(|err| {
        eprintln!("meridian-launcher: {err}; using defaults");
        Config::default()
    });
    let pinned_ids: Vec<String> = config.dock.pinned.iter().map(|id| id.to_lowercase()).collect();
    let entries = Rc::new(apps::installed());

    // ---- widgets ----------------------------------------------------------
    let search = gtk::Entry::builder()
        .placeholder_text("Search apps")
        .primary_icon_name("system-search-symbolic")
        .css_classes(["search"])
        .build();

    let pinned_label = section_label("Pinned");
    let pinned_grid = app_grid();
    let all_label = section_label("All apps");
    let all_grid = app_grid();
    let empty_label = gtk::Label::builder()
        .label("No apps found")
        .css_classes(["empty"])
        .visible(false)
        .build();

    // `launch` is shared by every app button and by Enter in the search field.
    let launch: Rc<dyn Fn(&gio::AppInfo)> = {
        let window = window.clone();
        Rc::new(move |info: &gio::AppInfo| {
            // The launch context passes focus/startup information along.
            let context = gdk::Display::default().map(|display| display.app_launch_context());
            if let Err(err) = info.launch(&[], context.as_ref()) {
                eprintln!("meridian-launcher: could not start {}: {err}", info.name());
            }
            window.set_visible(false);
        })
    };

    for (index, entry) in entries.iter().enumerate() {
        // The widget name carries the entry's index, for filtering below.
        all_grid.insert(&app_button(entry, index, &launch), -1);
    }
    for id in &pinned_ids {
        if let Some((index, entry)) = entries.iter().enumerate().find(|(_, entry)| &entry.id == id) {
            pinned_grid.insert(&app_button(entry, index, &launch), -1);
        }
    }

    let recent_label = section_label("Recent");
    let recent_list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let has_recent = fill_recent(&recent_list, &window);

    let scrolled_content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for widget in [
        pinned_label.upcast_ref::<gtk::Widget>(),
        pinned_grid.upcast_ref(),
        all_label.upcast_ref(),
        all_grid.upcast_ref(),
        empty_label.upcast_ref(),
        recent_label.upcast_ref(),
        recent_list.upcast_ref(),
    ] {
        scrolled_content.append(widget);
    }
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&scrolled_content)
        .build();

    let panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["panel"])
        .width_request(PANEL_WIDTH)
        .height_request(PANEL_HEIGHT)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::End)
        .build();
    panel.append(&search);
    panel.append(&scroller);
    panel.append(&footer(&window));

    // An Overlay stacks widgets: the click-catching background below, the panel on top.
    let background = gtk::Box::builder().hexpand(true).vexpand(true).build();
    let click_outside = gtk::GestureClick::new();
    click_outside.connect_pressed(glib::clone!(
        #[weak]
        window,
        move |_, _, _, _| window.set_visible(false)
    ));
    background.add_controller(click_outside);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&background));
    overlay.add_overlay(&panel);
    window.set_child(Some(&overlay));

    // ---- search -----------------------------------------------------------
    // The current query, shared between the filter and the handlers.
    let query = Rc::new(RefCell::new(String::new()));

    all_grid.set_filter_func({
        let entries = Rc::clone(&entries);
        let query = Rc::clone(&query);
        move |child| {
            entry_index(child)
                .and_then(|index| entries.get(index))
                .is_some_and(|entry| entry.matches(&query.borrow()))
        }
    });

    search.connect_changed({
        let entries = Rc::clone(&entries);
        let query = Rc::clone(&query);
        let all_grid = all_grid.clone();
        move |search| {
            *query.borrow_mut() = search.text().to_string();
            let searching = !query.borrow().trim().is_empty();
            let any_match = entries.iter().any(|entry| entry.matches(&query.borrow()));

            // While searching only the results are shown.
            pinned_label.set_visible(!searching);
            pinned_grid.set_visible(!searching);
            recent_label.set_visible(!searching && has_recent);
            recent_list.set_visible(!searching && has_recent);
            all_label.set_label(if searching { "Results" } else { "All apps" });
            all_label.set_visible(any_match);
            empty_label.set_visible(!any_match);
            all_grid.invalidate_filter(); // re-run the filter function
        }
    });

    // Enter starts the first result.
    search.connect_activate({
        let entries = Rc::clone(&entries);
        let query = Rc::clone(&query);
        let launch = Rc::clone(&launch);
        move |_| {
            if let Some(entry) = entries.iter().find(|entry| entry.matches(&query.borrow())) {
                launch(&entry.info);
            }
        }
    });

    // Escape closes the menu.
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        window,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, _| {
            if key == gdk::Key::Escape {
                window.set_visible(false);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        }
    ));
    window.add_controller(keys);

    // Every time the menu opens: empty search field, cursor in it.
    window.connect_show(move |_| {
        search.set_text("");
        search.grab_focus();
    });

    window
}

fn load_css() {
    let css = format!("{}\n{}", theme::LIGHT.css_variables(), include_str!("style.css"));
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

fn section_label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes(["section"])
        .halign(gtk::Align::Start)
        .build()
}

/// A grid that wraps its children into rows of `COLUMNS`.
fn app_grid() -> gtk::FlowBox {
    gtk::FlowBox::builder()
        .min_children_per_line(COLUMNS)
        .max_children_per_line(COLUMNS)
        .homogeneous(true)
        .selection_mode(gtk::SelectionMode::None)
        .valign(gtk::Align::Start)
        .build()
}

fn app_button(entry: &AppEntry, index: usize, launch: &Rc<dyn Fn(&gio::AppInfo)>) -> gtk::FlowBoxChild {
    let image = match entry.info.icon() {
        Some(icon) => gtk::Image::from_gicon(&icon),
        None => gtk::Image::from_icon_name("application-x-executable"),
    };
    image.set_pixel_size(28);
    let icon_tile = gtk::Box::builder()
        .css_classes(["app-icon"])
        .halign(gtk::Align::Center)
        .build();
    image.set_hexpand(true);
    icon_tile.append(&image);

    let label = gtk::Label::builder()
        .label(&entry.name)
        .css_classes(["app-label"])
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(10)
        .build();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&icon_tile);
    content.append(&label);

    let button = gtk::Button::builder()
        .child(&content)
        .css_classes(["app"])
        .tooltip_text(&entry.name)
        .build();
    let launch = Rc::clone(launch);
    let info = entry.info.clone();
    button.connect_clicked(move |_| launch(&info));

    let child = gtk::FlowBoxChild::builder().child(&button).focusable(false).build();
    child.set_widget_name(&index.to_string());
    child
}

/// The index into the app list that `app_button` stored in the widget name.
fn entry_index(child: &gtk::FlowBoxChild) -> Option<usize> {
    child.widget_name().parse().ok()
}

/// Up to three recently used files, newest first. Returns false if there are none.
fn fill_recent(list: &gtk::Box, window: &gtk::ApplicationWindow) -> bool {
    let mut items = gtk::RecentManager::default().items();
    items.retain(|item| item.exists());
    items.sort_by_key(|item| std::cmp::Reverse(item.modified().to_unix()));

    for item in items.iter().take(3) {
        let name = gtk::Label::builder()
            .label(item.display_name())
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        let when = gtk::Label::builder()
            .label(apps::relative_day(&item.modified()))
            .css_classes(["when"])
            .build();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.append(&name);
        row.append(&when);

        let button = gtk::Button::builder().child(&row).css_classes(["recent"]).build();
        let uri = item.uri();
        button.connect_clicked(glib::clone!(
            #[weak]
            window,
            move |_| {
                let context = gdk::Display::default().map(|display| display.app_launch_context());
                if let Err(err) = gio::AppInfo::launch_default_for_uri(&uri, context.as_ref()) {
                    eprintln!("meridian-launcher: could not open {uri}: {err}");
                }
                window.set_visible(false);
            }
        ));
        list.append(&button);
    }
    !items.is_empty()
}

/// Bottom row: user name and "Power", which swaps in the power actions.
fn footer(window: &gtk::ApplicationWindow) -> gtk::Stack {
    let stack = gtk::Stack::builder()
        .css_classes(["footer"])
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();

    // Page 1: who is logged in.
    // The full name from the user database, or the login name if none is set.
    let real_name = glib::real_name().to_string_lossy().into_owned();
    let name = if real_name.is_empty() || real_name == "Unknown" {
        glib::user_name().to_string_lossy().into_owned()
    } else {
        real_name
    };
    let user = gtk::Label::builder()
        .label(name)
        .css_classes(["user"])
        .halign(gtk::Align::Start)
        .hexpand(true)
        .build();
    let power = gtk::Button::with_label("Power");
    let user_page = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    user_page.append(&user);
    user_page.append(&power);
    stack.add_named(&user_page, Some("user"));

    // Page 2: power actions.
    let power_page = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let back = gtk::Button::with_label("Back");
    back.set_hexpand(true);
    back.set_halign(gtk::Align::Start);
    power_page.append(&back);

    let log_out = gtk::Button::with_label("Log out");
    log_out.connect_clicked(|_| {
        // Ending the session means asking the compositor to quit.
        let sent = Connection::connect().and_then(|mut connection| connection.send(&Request::Action(Action::Quit)));
        if let Err(err) = sent {
            eprintln!("meridian-launcher: could not reach the compositor: {err}");
        }
    });
    power_page.append(&log_out);

    for (label, verb) in [("Suspend", "suspend"), ("Restart", "reboot"), ("Shut down", "poweroff")] {
        let button = gtk::Button::with_label(label);
        button.connect_clicked(glib::clone!(
            #[weak]
            window,
            move |_| {
                // systemd-logind lets the logged-in user do this without root.
                if let Err(err) = std::process::Command::new("systemctl").arg(verb).spawn() {
                    eprintln!("meridian-launcher: systemctl {verb} failed: {err}");
                }
                window.set_visible(false);
            }
        ));
        power_page.append(&button);
    }
    stack.add_named(&power_page, Some("power"));

    power.connect_clicked(glib::clone!(
        #[weak]
        stack,
        move |_| stack.set_visible_child_name("power")
    ));
    back.connect_clicked(glib::clone!(
        #[weak]
        stack,
        move |_| stack.set_visible_child_name("user")
    ));
    // Always start on the user page.
    window.connect_show(glib::clone!(
        #[weak]
        stack,
        move |_| stack.set_visible_child_name("user")
    ));

    stack
}
