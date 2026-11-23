//! The D-Bus side: the `org.freedesktop.Notifications` interface from the
//! Desktop Notifications Specification, implemented with zbus.

use std::{
    collections::HashMap,
    sync::atomic::{AtomicU32, Ordering},
};

use zbus::{Connection, OwnedVariant};

const BUS_NAME: &str = "org.freedesktop.Notifications";
const OBJECT_PATH: &str = "/org/freedesktop/Notifications";

/// One notification, as received from an application.
#[derive(Debug, Clone)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    /// An icon name from the icon theme, or an absolute file path, or empty.
    pub icon: String,
    pub summary: String,
    pub body: String,
    /// Milliseconds; 0 = never expire, -1 = let the daemon decide.
    pub expire_timeout: i32,
}

/// What the D-Bus thread tells the GTK thread.
#[derive(Debug)]
pub enum Event {
    Show(Notification),
    Close(u32),
}

/// Why a notification went away; the numbers are fixed by the specification.
#[derive(Debug, Clone, Copy)]
pub enum CloseReason {
    Expired = 1,
    Dismissed = 2,
    ClosedByCall = 3,
}

struct Server {
    /// `AtomicU32`: a counter that is safe to change from any thread.
    next_id: AtomicU32,
    events: async_channel::Sender<Event>,
}

// This attribute generates the D-Bus plumbing. Each method becomes a D-Bus
// method with its name in CamelCase (`notify` is called as `Notify`).
#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    /// Which optional features we support. "body" = we show body text.
    fn get_capabilities(&self) -> Vec<&'static str> {
        vec!["body"]
    }

    #[allow(clippy::too_many_arguments)] // the argument list is fixed by the specification
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        _actions: Vec<String>,
        hints: HashMap<String, OwnedVariant>,
        expire_timeout: i32,
    ) -> u32 {
        // 0 means "new notification"; anything else updates an existing one.
        let id = if replaces_id != 0 {
            replaces_id
        } else {
            self.next_id.fetch_add(1, Ordering::Relaxed)
        };

        // Applications may pass the icon as an argument or as a "hint".
        let hint_icon = ["image-path", "image_path"]
            .iter()
            .find_map(|key| hints.get(*key))
            .and_then(|value| value.downcast_ref::<&str>().ok())
            .map(|path| path.trim_start_matches("file://").to_string());
        let icon = if app_icon.is_empty() { hint_icon.unwrap_or_default() } else { app_icon };

        let notification = Notification {
            id,
            app_name,
            icon,
            summary,
            body: strip_markup(&body),
            expire_timeout,
        };
        // `send_blocking` on an unbounded channel returns immediately.
        let _ = self.events.send_blocking(Event::Show(notification));
        id
    }

    fn close_notification(&self, id: u32) {
        let _ = self.events.send_blocking(Event::Close(id));
    }

    /// Name, vendor, version, and the specification version we implement.
    fn get_server_information(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        ("Meridian", "Meridian", env!("CARGO_PKG_VERSION"), "1.2")
    }
}

/// Connect to the session bus and claim the notifications name.
/// Fails if another notification daemon already owns it.
pub fn start(events: async_channel::Sender<Event>) -> zbus::Result<Connection> {
    let server = Server {
        next_id: AtomicU32::new(1),
        events,
    };
    // zbus is async: `build()` returns a future. `block_on` runs it to
    // completion right here, which is fine once at startup. Afterwards zbus
    // serves the connection from its own background thread.
    zbus::block_on(
        zbus::connection::Builder::session()
            .name(BUS_NAME)
            .serve_at(OBJECT_PATH, server)
            .build(),
    )
}

/// Send the `NotificationClosed` signal.
pub async fn emit_closed(connection: Connection, id: u32, reason: CloseReason) {
    let result = connection
        .emit_signal(
            None::<&str>, // broadcast to everyone
            OBJECT_PATH,
            BUS_NAME,
            "NotificationClosed",
            &(id, reason as u32),
        )
        .await;
    if let Err(err) = result {
        eprintln!("meridian-notify: could not send NotificationClosed: {err}");
    }
}

/// Bodies may contain simple markup such as `<b>`. We show plain text, so
/// drop the tags and decode the few entities the specification allows.
fn strip_markup(body: &str) -> String {
    let mut text = String::with_capacity(body.len());
    let mut in_tag = false;
    for ch in body.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::strip_markup;

    #[test]
    fn markup_is_removed() {
        assert_eq!(strip_markup("Build <b>finished</b> in 3 s"), "Build finished in 3 s");
        assert_eq!(strip_markup("a &lt; b &amp;&amp; c"), "a < b && c");
        assert_eq!(strip_markup("plain"), "plain");
    }
}
