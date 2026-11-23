//! Meridian's notification daemon.
//!
//! Applications do not draw their own notifications. They call the `Notify`
//! method of whichever program owns the D-Bus name
//! `org.freedesktop.Notifications`; that program (this one) shows them.
//!
//! Two threads are involved:
//!
//! * zbus runs the D-Bus connection on its own background thread and calls
//!   our `Server` methods there;
//! * GTK widgets may only be touched from the main thread.
//!
//! So `Server` does not touch widgets. It sends an `Event` through a channel,
//! and the main thread receives it and updates the popups.

mod server;

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use gtk::{gdk, glib, prelude::*};
use gtk4_layer_shell::{Edge, Layer, LayerShell};
use meridian_common::{
    ipc::{Connection, State},
    theme,
};

use crate::server::{CloseReason, Event, Notification};

/// How long a popup stays when the application does not say.
const DEFAULT_TIMEOUT_MS: u32 = 5000;
/// Older popups are dropped beyond this many.
const MAX_VISIBLE: usize = 5;
const CARD_WIDTH: i32 = 340;

fn main() -> glib::ExitCode {
    // Take the bus name before starting GTK: if another notification daemon
    // is already running there is nothing for us to do.
    let (sender, receiver) = async_channel::unbounded::<Event>();
    let connection = match server::start(sender) {
        Ok(connection) => connection,
        Err(err) => {
            eprintln!("meridian-notify: cannot become the notification daemon: {err}");
            return glib::ExitCode::FAILURE;
        }
    };

    let app = gtk::Application::builder()
        .application_id("org.meridian.Notify")
        .build();

    app.connect_activate(move |app| {
        if !app.windows().is_empty() {
            return; // activated a second time
        }
        let popups = Popups::new(app, connection.clone());

        // `spawn_future_local` runs an async block on the GTK main thread.
        // `recv().await` waits for the next event without blocking the UI.
        let receiver = receiver.clone();
        glib::spawn_future_local(async move {
            while let Ok(event) = receiver.recv().await {
                match event {
                    Event::Show(notification) => popups.show(notification),
                    Event::Close(id) => popups.close(id, CloseReason::ClosedByCall),
                }
            }
        });
    });

    // No window is visible most of the time; keep running anyway.
    app.connect_startup(|app| std::mem::forget(app.hold()));
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

/// The popup window and the cards currently in it.
struct Popups {
    window: gtk::ApplicationWindow,
    stack: gtk::Box,
    cards: RefCell<HashMap<u32, Card>>,
    connection: zbus::Connection,
}

struct Card {
    widget: gtk::Button,
    /// The pending auto-close timer, if the notification expires.
    timer: Option<glib::SourceId>,
}

impl Popups {
    fn new(app: &gtk::Application, connection: zbus::Connection) -> Rc<Self> {
        load_css();

        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .css_classes(["notify-window"])
            .build();

        // Top-right corner, above everything. Without an exclusive zone of
        // its own the compositor places it below the bar.
        window.init_layer_shell();
        window.set_namespace(Some("meridian-notify"));
        window.set_layer(Layer::Overlay);
        window.set_anchor(Edge::Top, true);
        window.set_anchor(Edge::Right, true);

        let stack = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["stack"])
            .build();
        window.set_child(Some(&stack));

        Rc::new(Self {
            window,
            stack,
            cards: RefCell::new(HashMap::new()),
            connection,
        })
    }

    fn show(self: &Rc<Self>, notification: Notification) {
        let id = notification.id;
        // "Replaces" an existing popup, e.g. a volume level that keeps changing.
        self.remove(id);

        let icon = if notification.icon.starts_with('/') {
            gtk::Image::from_file(&notification.icon)
        } else if notification.icon.is_empty() {
            gtk::Image::from_icon_name("preferences-system-notifications-symbolic")
        } else {
            gtk::Image::from_icon_name(&notification.icon)
        };
        icon.set_valign(gtk::Align::Start);

        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        if !notification.app_name.is_empty() {
            text.append(&text_label(&notification.app_name, "app"));
        }
        text.append(&text_label(&notification.summary, "summary"));
        if !notification.body.is_empty() {
            text.append(&text_label(&notification.body, "body"));
        }

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        content.append(&icon);
        content.append(&text);

        let widget = gtk::Button::builder()
            .child(&content)
            .css_classes(["card"])
            .width_request(CARD_WIDTH)
            .build();
        // Clicking a popup dismisses it. A weak reference avoids a cycle
        // (the card would keep `Popups` alive and the other way around).
        let popups = Rc::downgrade(self);
        widget.connect_clicked(move |_| {
            if let Some(popups) = popups.upgrade() {
                popups.close(id, CloseReason::Dismissed);
            }
        });

        // A timeout of 0 means "stay until dismissed"; negative means "you decide".
        let timeout_ms = match notification.expire_timeout {
            0 => None,
            ms if ms < 0 => Some(DEFAULT_TIMEOUT_MS),
            ms => Some(ms as u32),
        };
        let timer = timeout_ms.map(|ms| {
            let popups = Rc::downgrade(self);
            glib::timeout_add_local_once(std::time::Duration::from_millis(ms.into()), move || {
                if let Some(popups) = popups.upgrade() {
                    // The timer has fired: forget it so `remove` does not cancel it again.
                    if let Some(card) = popups.cards.borrow_mut().get_mut(&id) {
                        card.timer = None;
                    }
                    popups.close(id, CloseReason::Expired);
                }
            })
        });

        self.stack.prepend(&widget); // newest on top
        self.cards.borrow_mut().insert(id, Card { widget, timer });

        // Too many: drop the oldest (lowest id).
        while self.cards.borrow().len() > MAX_VISIBLE {
            let oldest = self.cards.borrow().keys().min().copied();
            match oldest {
                Some(oldest) => self.close(oldest, CloseReason::Expired),
                None => break,
            }
        }

        self.window.present();
    }

    /// Remove a popup and tell the application why, as the protocol requires.
    fn close(&self, id: u32, reason: CloseReason) {
        if self.remove(id) {
            // Sending is async; let it finish in the background of the main loop.
            glib::spawn_future_local(server::emit_closed(self.connection.clone(), id, reason));
        }
    }

    /// Take a popup off the screen. Returns false if it was not there.
    fn remove(&self, id: u32) -> bool {
        let Some(card) = self.cards.borrow_mut().remove(&id) else {
            return false;
        };
        if let Some(timer) = card.timer {
            timer.remove();
        }
        self.stack.remove(&card.widget);
        if self.cards.borrow().is_empty() {
            self.window.set_visible(false);
        }
        true
    }
}

fn text_label(text: &str, class: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes([class])
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .max_width_chars(36)
        .lines(4)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build()
}

fn load_css() {
    let css = format!("{}\n{}", theme::LIGHT.css_variables(), include_str!("style.css"));
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}
