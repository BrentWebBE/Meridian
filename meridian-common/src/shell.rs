//! Helpers shared by the GTK programs (bar, dock, Find, notifications,
//! Settings): themed stylesheets and the connection to the compositor.
//!
//! Only compiled with the `gtk` feature, so the compositor does not pull in GTK.

use std::{cell::RefCell, rc::Rc};

use gtk::{gdk, glib, prelude::*};

use crate::{
    config::Config,
    ipc::{Connection, Request, State},
    theme::{Mood, Themes},
};

/// A program's stylesheet, re-themeable while it runs.
pub struct Styles {
    provider: gtk::CssProvider,
    /// The program's own CSS, which refers to the theme's `--m-*` variables.
    css: &'static str,
    current: RefCell<String>,
}

impl Styles {
    /// Install `css` for the whole program, with the theme from the config.
    pub fn install(css: &'static str) -> Rc<Self> {
        let styles = Rc::new(Self {
            provider: gtk::CssProvider::new(),
            css,
            current: RefCell::new(String::new()),
        });
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &styles.provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        // Until the compositor tells us otherwise, work it out ourselves.
        let config = Config::load().unwrap_or_default();
        styles.apply(config.appearance.current_mood(current_hour()).name());
        styles
    }

    /// Switch to the named theme. Cheap to call repeatedly with the same name.
    pub fn apply(&self, theme_name: &str) {
        if *self.current.borrow() == theme_name {
            return;
        }
        let theme = Themes::load().get(Mood::from_name(theme_name));
        // Theme variables first, then the stylesheet that uses them.
        self.provider
            .load_from_string(&format!("{}\n{}", theme.css_variables(), self.css));
        *self.current.borrow_mut() = theme_name.to_string();
    }

    pub fn current(&self) -> String {
        self.current.borrow().clone()
    }
}

// One stylesheet per program, reachable from any callback. GTK programs run
// their UI on a single thread, which is what `thread_local!` storage fits.
thread_local! {
    static STYLES: RefCell<Option<Rc<Styles>>> = const { RefCell::new(None) };
}

/// Install the program's stylesheet. Call once, when the UI is built.
pub fn install_css(css: &'static str) {
    let styles = Styles::install(css);
    STYLES.with(|slot| *slot.borrow_mut() = Some(styles));
}

/// Follow the compositor's mood ("morning", "dusk", "night"). Call this
/// with `state.theme` on every update; it does nothing if the mood is unchanged.
pub fn apply_mood(name: &str) {
    STYLES.with(|slot| {
        if let Some(styles) = slot.borrow().as_ref() {
            styles.apply(name);
        }
    });
}

/// The mood currently applied ("morning", "dusk" or "night").
pub fn current_mood() -> String {
    STYLES.with(|slot| slot.borrow().as_ref().map(|styles| styles.current()).unwrap_or_default())
}

/// Call `on_change` whenever the config file is written. The kernel's
/// inotify tells GIO, GIO tells us: no polling. Editors and our own
/// Settings app replace the file rather than modify it, so we watch the
/// folder and look for the file's name.
pub fn watch_config(on_change: impl Fn() + 'static) {
    let Some(path) = Config::path() else { return };
    let Some(folder) = path.parent() else { return };
    let _ = std::fs::create_dir_all(folder);

    let directory = gtk::gio::File::for_path(folder);
    let monitor = match directory.monitor_directory(gtk::gio::FileMonitorFlags::NONE, gtk::gio::Cancellable::NONE) {
        Ok(monitor) => monitor,
        Err(err) => {
            eprintln!("cannot watch {}: {err}", folder.display());
            return;
        }
    };
    monitor.connect_changed(move |_, file, _, event| {
        use gtk::gio::FileMonitorEvent as Event;
        let ours = file
            .basename()
            .is_some_and(|name| name.as_os_str() == "config.toml" || name.as_os_str() == "themes.toml");
        // "ChangesDoneHint" marks the end of a write; "Renamed"/"MovedIn"
        // is how an atomic replace shows up.
        if ours && matches!(event, Event::ChangesDoneHint | Event::Renamed | Event::MovedIn | Event::Created) {
            on_change();
        }
    });
    // The monitor stops when dropped; it must live as long as the program.
    std::mem::forget(monitor);
}

/// True if the user asked for reduced motion (GTK's animation setting).
pub fn reduced_motion() -> bool {
    gtk::Settings::default().is_some_and(|settings| !settings.is_gtk_enable_animations())
}

/// The hour of the day (0-23) in local time.
pub fn current_hour() -> u32 {
    glib::DateTime::now_local().map(|now| now.hour() as u32).unwrap_or(12)
}

/// Call `on_state` with every update from the compositor, and `on_closed`
/// once when the compositor goes away. Returns false if we are not running
/// under Meridian.
pub fn watch_compositor(on_state: impl Fn(&State) + 'static, on_closed: impl FnOnce() + 'static) -> bool {
    let Ok(connection) = Connection::connect() else {
        return false;
    };
    let fd = connection.raw_fd();
    let connection = RefCell::new(connection);
    let mut on_closed = Some(on_closed);

    // GLib calls this on the main thread whenever the socket has data.
    glib_unix::unix_fd_add_local(fd, glib::IOCondition::IN | glib::IOCondition::HUP, move |_, _| {
        // Bind the result first so the `RefCell` borrow ends before the
        // callbacks run (they may want to send a request).
        let received = connection.borrow_mut().receive::<State>();
        match received {
            Ok(states) => {
                // Only the newest snapshot matters.
                if let Some(state) = states.last() {
                    on_state(state);
                }
                glib::ControlFlow::Continue
            }
            Err(_) => {
                if let Some(on_closed) = on_closed.take() {
                    on_closed();
                }
                glib::ControlFlow::Break
            }
        }
    });
    true
}

/// Send one request to the compositor. Errors are reported on stderr.
pub fn send(request: &Request) {
    let sent = Connection::connect().and_then(|mut connection| connection.send(request));
    if let Err(err) = sent {
        eprintln!("could not reach the compositor: {err}");
    }
}
