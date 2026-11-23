//! Meridian's Settings app. Every change is written to
//! `~/.config/meridian/config.toml` right away; the compositor and the
//! shell programs watch that file and follow.

mod appearance;
mod launching;
mod preview;
mod widgets;

use gtk::{glib, prelude::*};
use meridian_common::shell;

fn main() -> glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id("org.meridian.Settings")
        .build();
    app.connect_activate(|app| match app.windows().first() {
        // Already open: bring it forward instead of opening a second window.
        Some(window) => window.present(),
        None => build_ui(app),
    });
    app.run()
}

fn build_ui(app: &gtk::Application) {
    shell::install_css(include_str!("style.css"));

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Settings")
        .default_width(940)
        .default_height(620)
        .css_classes(["settings"])
        .build();

    // Our own title bar: title on the left, window buttons on the right.
    let header = gtk::HeaderBar::builder()
        .show_title_buttons(true)
        .decoration_layout(":minimize,maximize,close")
        .title_widget(&gtk::Box::new(gtk::Orientation::Horizontal, 0)) // no centered title
        .build();
    header.pack_start(&gtk::Label::builder().label("Settings").css_classes(["title"]).build());
    window.set_titlebar(Some(&header));

    // ---- pages ------------------------------------------------------------
    let stack = gtk::Stack::builder()
        .hexpand(true)
        .transition_type(if shell::reduced_motion() {
            gtk::StackTransitionType::None
        } else {
            gtk::StackTransitionType::Crossfade
        })
        .build();
    let sidebar = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["sidebar"])
        .width_request(210)
        .build();

    let pages: [(&str, gtk::Box); 6] = [
        ("Appearance", appearance::page()),
        ("Launching apps", launching::page()),
        ("Displays", not_yet("Displays")),
        ("Network", not_yet("Network")),
        ("Sound", not_yet("Sound")),
        ("Users", not_yet("Users")),
    ];
    let mut first: Option<gtk::ToggleButton> = None;
    for (title, page) in pages {
        stack.add_named(&page, Some(title));

        let button = gtk::ToggleButton::builder().label(title).build();
        // Left-align the text inside the button.
        if let Some(label) = button.child().and_downcast::<gtk::Label>() {
            label.set_xalign(0.0);
        }
        button.set_group(first.as_ref());
        button.connect_toggled(glib::clone!(
            #[weak]
            stack,
            move |button| {
                if button.is_active() {
                    stack.set_visible_child_name(title);
                }
            }
        ));
        sidebar.append(&button);
        first.get_or_insert_with(|| button.clone());
    }
    if let Some(first) = &first {
        first.set_active(true);
    }

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.append(&sidebar);
    content.append(&stack);
    window.set_child(Some(&content));

    // Follow the compositor's mood, so Settings itself changes with it.
    shell::watch_compositor(
        glib::clone!(
            #[weak]
            window,
            move |state| {
                shell::apply_mood(&state.theme);
                window.queue_draw(); // the previews use the mood's colors
            }
        ),
        || {},
    );

    window.present();
}

/// A category that exists in the design but is not built.
fn not_yet(title: &str) -> gtk::Box {
    widgets::page(title, "This page is not available yet.")
}
