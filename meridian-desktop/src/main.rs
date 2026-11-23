//! Meridian's desktop icons: Home, Documents, Projects and Trash in a
//! column at the top left.
//!
//! This is a layer surface on the "bottom" layer: above the wallpaper,
//! below every window.

use std::path::PathBuf;

use gtk::{gdk, gio, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use meridian_common::{config::Config, shell};

fn main() -> glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id("org.meridian.Desktop")
        .build();
    app.connect_activate(build_ui);
    app.run()
}

/// One icon: what it is called, how it looks, and what it opens (a URI).
struct Place {
    name: &'static str,
    icon: &'static str,
    uri: String,
}

/// The places to show. Folders that do not exist are left out.
fn places() -> Vec<Place> {
    let home = glib::home_dir();
    let folder = |name: &'static str, icon: &'static str, path: Option<PathBuf>| {
        let path = path.filter(|path| path.is_dir())?;
        Some(Place {
            name,
            icon,
            uri: gio::File::for_path(path).uri().to_string(),
        })
    };
    // "Projects" has no standard location; accept either spelling.
    let projects = ["Projects", "projects"]
        .iter()
        .map(|name| home.join(name))
        .find(|path| path.is_dir());

    [
        folder("Home", "user-home-symbolic", Some(home.clone())),
        folder(
            "Documents",
            "folder-documents-symbolic",
            glib::user_special_dir(glib::UserDirectory::Documents),
        ),
        folder("Projects", "folder-symbolic", projects),
        // The trash is not a folder but a location every file manager knows.
        Some(Place {
            name: "Trash",
            icon: "user-trash-symbolic",
            uri: "trash:///".into(),
        }),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn build_ui(app: &gtk::Application) {
    shell::install_css(include_str!("style.css"));

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .css_classes(["desktop-window"])
        .build();
    window.init_layer_shell();
    window.set_namespace(Some("meridian-desktop"));
    window.set_layer(Layer::Bottom);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Left, true);
    // Icons can be reached with the keyboard after a click on one.
    window.set_keyboard_mode(KeyboardMode::OnDemand);

    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["icons"])
        .build();

    for place in places() {
        let tile = gtk::Box::builder()
            .css_classes(["icon-tile"])
            .halign(gtk::Align::Center)
            .build();
        let image = gtk::Image::from_icon_name(place.icon);
        image.set_hexpand(true);
        tile.append(&image);

        let label = gtk::Label::builder()
            .label(place.name)
            .css_classes(["icon-label"])
            .halign(gtk::Align::Center)
            .build();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&tile);
        content.append(&label);

        let button = gtk::Button::builder()
            .child(&content)
            .css_classes(["desktop-icon"])
            .tooltip_text(format!("Open {}", place.name))
            .build();
        button.connect_clicked(move |_| {
            // Opens the location in the default file manager.
            let context = gdk::Display::default().map(|display| display.app_launch_context());
            if let Err(err) = gio::AppInfo::launch_default_for_uri(&place.uri, context.as_ref()) {
                eprintln!("meridian-desktop: could not open {}: {err}", place.uri);
            }
        });
        column.append(&button);
    }
    window.set_child(Some(&column));

    // Follow the mood; quit with the compositor.
    shell::watch_compositor(
        |state| shell::apply_mood(&state.theme),
        glib::clone!(
            #[weak]
            app,
            move || app.quit()
        ),
    );

    // `[desktop] icons = false` hides them, live.
    let show = glib::clone!(
        #[weak]
        window,
        move || {
            let visible = Config::load().map(|config| config.desktop.icons).unwrap_or(true);
            window.set_visible(visible);
        }
    );
    shell::watch_config(show.clone());
    show();
}
