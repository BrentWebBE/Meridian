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

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

use gtk::{glib, prelude::*};
use gtk4_layer_shell::{Edge, Layer, LayerShell};
use meridian_common::{
    ipc::{Connection, State},
    shell,
};

use crate::server::{CloseReason, Event, Notification, Urgency};

/// How long a popup stays when the application does not say.
const DEFAULT_TIMEOUT_MS: u32 = 5000;
/// Older popups are dropped beyond this many.
const MAX_VISIBLE: usize = 5;
const CARD_WIDTH: i32 = 320;

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
                    Event::MarkRead => popups.set_unread(0),
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
        // Of the updates we only use the mood; a closed socket means quit.
        let received = connection.borrow_mut().receive::<State>();
        match received {
            Ok(states) => {
                if let Some(state) = states.last() {
                    shell::apply_mood(&state.theme);
                }
                glib::ControlFlow::Continue
            }
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
    /// Notifications that timed out without being clicked.
    unread: Cell<u32>,
    connection: zbus::Connection,
}

struct Card {
    widget: gtk::Box,
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
            unread: Cell::new(0),
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

        // The card: content on top, action buttons (if any) below.
        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["card"])
            .width_request(CARD_WIDTH)
            .build();
        if notification.urgency == Urgency::Critical {
            widget.add_css_class("critical");
        }
        widget.append(&content);

        // One button per action. "default" is not a button: it is what a
        // click on the card itself means.
        let has_default = notification.actions.iter().any(|(key, _)| key == "default");
        let buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .css_classes(["actions"])
            .build();
        for (key, label) in notification.actions.iter().filter(|(key, _)| key != "default") {
            let button = gtk::Button::builder().label(label).css_classes(["action"]).build();
            // A weak reference avoids a cycle (the card would keep
            // `Popups` alive and the other way around).
            let popups = Rc::downgrade(self);
            let key = key.clone();
            button.connect_clicked(move |_| {
                if let Some(popups) = popups.upgrade() {
                    popups.invoke(id, &key);
                }
            });
            buttons.append(&button);
        }
        if buttons.first_child().is_some() {
            widget.append(&buttons);
        }

        // A click on the card: run the default action if there is one,
        // and dismiss the popup either way.
        let click = gtk::GestureClick::new();
        let popups = Rc::downgrade(self);
        click.connect_released(move |_, _, _, _| {
            if let Some(popups) = popups.upgrade() {
                if has_default {
                    popups.invoke(id, "default");
                } else {
                    popups.close(id, CloseReason::Dismissed);
                }
            }
        });
        content.add_controller(click);

        // A timeout of 0 means "stay until dismissed"; negative means "you decide".
        // Critical ones stay regardless.
        let timeout_ms = match notification.expire_timeout {
            _ if notification.urgency == Urgency::Critical => None,
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

    /// The user chose an action: tell the application, then close the popup.
    fn invoke(&self, id: u32, key: &str) {
        if self.cards.borrow().contains_key(&id) {
            glib::spawn_future_local(server::emit_action(self.connection.clone(), id, key.to_string()));
            self.close(id, CloseReason::Dismissed);
        }
    }

    fn set_unread(&self, unread: u32) {
        self.unread.set(unread);
        glib::spawn_future_local(server::emit_unread(self.connection.clone(), unread));
    }

    /// Remove a popup and tell the application why, as the protocol requires.
    fn close(&self, id: u32, reason: CloseReason) {
        if self.remove(id) {
            // It went away on its own: you may not have seen it.
            if matches!(reason, CloseReason::Expired) {
                self.set_unread(self.unread.get() + 1);
            }
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
    // Theme variables plus our stylesheet; re-themed when the mood changes.
    shell::install_css(include_str!("style.css"));
}
